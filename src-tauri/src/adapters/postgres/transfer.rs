//! PostgreSQL 的对比/导出/迁移方言（A5）：逐项 ALTER COLUMN、枚举换型经 text、枚举加标签与序列原地改写、
//! 视图/例程成环时先换成桩、按模式限定，会话只连一个库。

use crate::adapters::transfer::{compare_mismatch, constraint_primary_key, dump_mismatch, dump_session_gap, qualify, read_failure, InPlaceTypes, SequenceSpec, TableAlter, TransferDialect, ViewReplace};
use crate::adapters::{quote_ident, ErrorRules};
use crate::compare::model::{Kind, ObjectDef, Side};
use crate::compare::structure::{column_clause, type_mentions};
use crate::connections::model::Engine;
use crate::connections::CatalogSession;

const ENGINE: Engine = Engine::Postgres;

fn quote(name: &str) -> String {
    quote_ident(ENGINE, name)
}

fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(in crate::adapters) struct PostgresTransfer;

#[async_trait::async_trait]
impl TransferDialect for PostgresTransfer {
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
                None => statements.push(format!("ALTER TABLE {table} ADD COLUMN {}", column_clause(ENGINE, column))),
                Some(other) => {
                    if !column.type_name.eq_ignore_ascii_case(&other.type_name) {
                        let using = enum_using(alter.baseline_side, alter.side, &other.type_name, &column.type_name, &column.name);
                        statements.push(format!("ALTER TABLE {table} ALTER COLUMN {} TYPE {}{using}", quote(&column.name), column.type_name.trim()));
                    }
                    if column.nullable != other.nullable {
                        let verb = if column.nullable { "DROP NOT NULL" } else { "SET NOT NULL" };
                        statements.push(format!("ALTER TABLE {table} ALTER COLUMN {} {verb}", quote(&column.name)));
                    }
                    if column.default_sql.trim() != other.default_sql.trim() {
                        if column.default_sql.trim().is_empty() {
                            statements.push(format!("ALTER TABLE {table} ALTER COLUMN {} DROP DEFAULT", quote(&column.name)));
                        } else {
                            statements.push(format!("ALTER TABLE {table} ALTER COLUMN {} SET DEFAULT {}", quote(&column.name), column.default_sql.trim()));
                        }
                    }
                }
            }
        }
        if alter.with_pk {
            constraint_primary_key(ENGINE, &table, name, baseline, target, &mut statements);
        }
        statements
    }

    fn drop_index(&self, side: &Side, _table: &str, index: &str) -> String {
        format!("DROP INDEX {}", qualify(ENGINE, side, index))
    }

    /// 外键指向别的模式时不算本批依赖。
    fn drop_order_same_schema_only(&self) -> bool {
        true
    }

    fn drop_trigger(&self, side: &Side, object: &ObjectDef) -> String {
        format!("DROP TRIGGER {} ON {}", quote(&object.name), qualify(ENGINE, side, &object.relation))
    }

    fn drop_routine(&self, side: &Side, word: &str, object: &ObjectDef) -> String {
        if object.signature.is_empty() {
            format!("DROP {word} {}", qualify(ENGINE, side, &object.name))
        } else {
            format!("DROP {word} {}({})", qualify(ENGINE, side, &object.name), object.signature)
        }
    }

    /// 触发器名不能带 schema，schema 只写在 ON 后面的表上。
    fn trigger_name_unqualified(&self) -> bool {
        true
    }

    fn drop_default(&self, table: &str, column: &str) -> String {
        format!("ALTER TABLE {table} ALTER COLUMN {} DROP DEFAULT", quote(column))
    }

    fn set_default(&self, table: &str, column: &str, expression: &str) -> String {
        format!("ALTER TABLE {table} ALTER COLUMN {} SET DEFAULT {expression}", quote(column))
    }

    /// 少列的 CREATE OR REPLACE VIEW 会被拒绝：只有能证明输出列兼容时才就地替换。
    fn view_replace(&self) -> ViewReplace {
        ViewReplace::MatchingColumns
    }

    fn replaces_routine_in_place(&self) -> bool {
        true
    }

    fn breaks_view_routine_cycles(&self) -> bool {
        true
    }

    fn routine_stub(&self, header: &str, procedure: bool, returns: &str) -> Option<String> {
        if procedure {
            return Some(format!("{header} LANGUAGE plpgsql AS $$ BEGIN NULL; END $$"));
        }
        let upper = returns.to_ascii_uppercase();
        if returns.is_empty() || upper.contains("SETOF") || upper.contains("TABLE") || upper == "VOID" {
            Some(format!("{header} LANGUAGE plpgsql AS $$ BEGIN RETURN; END $$"))
        } else {
            Some(format!("{header} LANGUAGE sql AS $$ SELECT NULL::{returns} $$"))
        }
    }

    fn in_place_types(&self) -> Option<&'static dyn InPlaceTypes> {
        Some(&PostgresTypes)
    }

    fn compare_scope_refusal(&self, baseline: &Side, target: &Side) -> Option<String> {
        (baseline.schema.trim().is_empty() || target.schema.trim().is_empty()).then(|| "PostgreSQL 需要为基准和对照各选一个 schema，不生成脚本。".to_owned())
    }

    /// 限定当前连接的库，schema 缺省全部。
    async fn dump_scope(&self, session: &CatalogSession, database: &str, schemas: &[String]) -> Result<(String, Vec<String>), String> {
        let current = text_column(session, "SELECT current_database()").await?.into_iter().next().ok_or_else(|| read_failure("没有当前数据库"))?;
        if !database.is_empty() && database != current {
            return Err(dump_session_gap(&current, database));
        }
        if !schemas.is_empty() {
            return Ok((current, schemas.to_vec()));
        }
        let sql = crate::catalog::metadata_sql(ENGINE).schemas.ok_or_else(|| read_failure("PostgreSQL 缺少 schema 查询"))?;
        let all = text_column(session, sql).await?;
        if all.is_empty() {
            return Err(read_failure("没有读到 schema"));
        }
        Ok((current, all))
    }

    async fn compare_scope(&self, session: &CatalogSession, _database: &str, schema: &str) -> Result<(), String> {
        if schema.trim().is_empty() {
            return Err("PostgreSQL 需要指定 schema".to_owned());
        }
        let CatalogSession::Postgres(client) = session else {
            return Err(compare_mismatch());
        };
        let rows = client.query("SELECT 1 FROM pg_namespace WHERE nspname = $1", &[&schema]).await.map_err(|err| crate::error_text::chain(&err))?;
        if rows.is_empty() {
            return Err(format!("schema {schema} 不存在"));
        }
        Ok(())
    }

    async fn session_database(&self, session: &CatalogSession) -> Result<String, String> {
        let CatalogSession::Postgres(client) = session else {
            return Ok(String::new());
        };
        let row = client.query_one("SELECT current_database()", &[]).await.map_err(|err| crate::error_text::chain(&err))?;
        let name: Option<String> = row.try_get(0).map_err(|err| crate::error_text::chain(&err))?;
        Ok(name.unwrap_or_default())
    }

    async fn schema_names(&self, session: &CatalogSession, sql: &str) -> Result<Vec<String>, String> {
        let CatalogSession::Postgres(client) = session else {
            return Err(compare_mismatch());
        };
        let rows = client.query(sql, &[]).await.map_err(|err| crate::error_text::chain(&err))?;
        let mut names = Vec::new();
        for row in &rows {
            let name = schema_name(row)?;
            if !name.is_empty() {
                names.push(name);
            }
        }
        Ok(names)
    }

    /// serial / identity 的序列由列拥有。relation 记成 `表\u{1}列`，结构脚本不再无条件 DROP SEQUENCE。
    async fn sequence_owner(&self, session: &CatalogSession, schema: &str, name: &str) -> Option<String> {
        let CatalogSession::Postgres(client) = session else {
            return None;
        };
        let sql = "SELECT tab.relname, att.attname FROM pg_class seq \
            JOIN pg_namespace ns ON ns.oid = seq.relnamespace \
            JOIN pg_depend dep ON dep.objid = seq.oid AND dep.deptype IN ('a', 'i') AND dep.classid = 'pg_class'::regclass AND dep.refclassid = 'pg_class'::regclass AND dep.refobjsubid > 0 \
            JOIN pg_class tab ON tab.oid = dep.refobjid \
            JOIN pg_attribute att ON att.attrelid = tab.oid AND att.attnum = dep.refobjsubid AND NOT att.attisdropped \
            WHERE ns.nspname = $1 AND seq.relname = $2 AND seq.relkind = 'S' \
            LIMIT 1";
        let rows = client.query(sql, &[&schema, &name]).await.ok()?;
        let row = rows.first()?;
        let table: String = row.try_get(0).ok()?;
        let column: String = row.try_get(1).ok()?;
        if table.is_empty() {
            return None;
        }
        if column.is_empty() {
            return Some(table);
        }
        Some(format!("{table}\u{1}{column}"))
    }
}

/// 两个枚举之间没有自动转型。经 text 转一次，标签对不上现有数据时由执行失败，而不是省略 USING。
fn enum_using(baseline: &Side, target: &Side, from_type: &str, to_type: &str, column: &str) -> String {
    if is_enum_type(target, from_type) && is_enum_type(baseline, to_type) {
        format!(" USING {}::text::{}", quote(column), to_type.trim())
    } else {
        String::new()
    }
}

fn is_enum_type(side: &Side, type_name: &str) -> bool {
    side.objects.iter().any(|object| object.kind == Kind::Enum && type_mentions(type_name, &object.name))
}

struct PostgresTypes;

impl InPlaceTypes for PostgresTypes {
    fn enum_add_value(&self, type_name: &str, label: &str, before: Option<&str>) -> String {
        let mut sql = format!("ALTER TYPE {type_name} ADD VALUE IF NOT EXISTS {}", quote_literal(label));
        if let Some(before) = before {
            sql.push_str(" BEFORE ");
            sql.push_str(&quote_literal(before));
        }
        sql
    }

    fn sequence_alter(&self, sequence: &str, spec: &SequenceSpec) -> String {
        let min_value = if spec.min_value.is_empty() { "NO MINVALUE".to_owned() } else { format!("MINVALUE {}", spec.min_value) };
        let max_value = if spec.max_value.is_empty() { "NO MAXVALUE".to_owned() } else { format!("MAXVALUE {}", spec.max_value) };
        let start = if spec.start.is_empty() { "1" } else { spec.start.as_str() };
        let cache = if spec.cache.is_empty() { "1" } else { spec.cache.as_str() };
        let cycle = if spec.cycle { "CYCLE" } else { "NO CYCLE" };
        format!("ALTER SEQUENCE {sequence} INCREMENT BY {} {min_value} {max_value} START WITH {start} CACHE {cache} {cycle}", spec.increment)
    }

    fn detach_column_type(&self, table: &str, column: &str) -> String {
        format!("ALTER TABLE {table} ALTER COLUMN {} TYPE text USING {}::text", quote(column), quote(column))
    }

    fn restore_column_type(&self, table: &str, column: &str, type_name: &str) -> String {
        format!("ALTER TABLE {table} ALTER COLUMN {} TYPE {type_name} USING {}::{type_name}", quote(column), quote(column))
    }

    fn sequence_owned_by(&self, sequence: &str, owner_table: &str, column: &str) -> String {
        format!("ALTER SEQUENCE {sequence} OWNED BY {owner_table}.{}", quote(column))
    }

    fn sequence_disown(&self, sequence: &str) -> String {
        format!("ALTER SEQUENCE {sequence} OWNED BY NONE")
    }
}

/// PostgreSQL 的写入冲突措辞：`duplicate key value violates unique constraint`。
pub(in crate::adapters) struct PostgresErrors;

impl ErrorRules for PostgresErrors {
    fn is_conflict(&self, lower: &str) -> bool {
        lower.contains("duplicate key")
    }
}

async fn text_column(session: &CatalogSession, sql: &str) -> Result<Vec<String>, String> {
    let CatalogSession::Postgres(client) = session else {
        return Err(dump_mismatch());
    };
    let rows = client.query(sql, &[]).await.map_err(read_failure)?;
    rows.iter()
        .map(|row| {
            if let Ok(value) = row.try_get::<_, String>(0) {
                return Ok(value);
            }
            match row.try_get::<_, Option<String>>(0) {
                Ok(Some(value)) => Ok(value),
                Ok(None) => Err(read_failure("元数据列是空值")),
                Err(err) => Err(read_failure(err)),
            }
        })
        .collect()
}

fn schema_name(row: &tokio_postgres::Row) -> Result<String, String> {
    if let Ok(value) = row.try_get::<_, String>(0) {
        return Ok(value);
    }
    match row.try_get::<_, Option<String>>(0) {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Err("schema 名是空值".to_owned()),
        Err(err) => Err(crate::error_text::chain(&err)),
    }
}

pub(in crate::adapters) async fn execute(client: &tokio_postgres::Client, statement: &str) -> Result<(), String> {
    client.execute(statement, &[]).await.map(|_| ()).map_err(|err| crate::error_text::chain(&err))
}

/// Arc<Client> 不支持 transaction()，同一条连接上的显式 BEGIN/COMMIT 等价。
pub(in crate::adapters) async fn insert_in_transaction(client: &tokio_postgres::Client, statement: &str) -> Result<(), String> {
    client.execute("BEGIN", &[]).await.map_err(|err| crate::error_text::chain(&err))?;
    match client.execute(statement, &[]).await {
        Ok(_) => client.execute("COMMIT", &[]).await.map(|_| ()).map_err(|err| crate::error_text::chain(&err)),
        Err(error) => {
            let _ = client.execute("ROLLBACK", &[]).await;
            Err(error.to_string())
        }
    }
}
