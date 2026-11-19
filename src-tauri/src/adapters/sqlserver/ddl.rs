//! SQL Server 的 DDL 方言：按模式限定、IDENTITY、sp_rename 改名、具名默认值约束、先删后建的视图替换与扩展属性注释。

use crate::adapters::ddl::{Comments, DdlDialect};
use crate::adapters::quote_ident;
use crate::connections::model::Engine;
use crate::connections::CatalogSession;
use crate::ddl::introspect::SESSION_MISMATCH;
use crate::ddl::model::{Column, ColumnDraft, Plan, Request, Risk, Snapshot, Step};
use crate::ddl::plan::{normalize_default, same_type, statement_alter_plan, Change};

use super::ddl_read;

pub(in crate::adapters) struct SqlserverDdl;

#[async_trait::async_trait]
impl DdlDialect for SqlserverDdl {
    fn qualified_table(&self, _database: &str, schema: &str, table: &str) -> Result<String, String> {
        if schema.is_empty() {
            return Err("SQL Server 需要指定 schema（表所在的分组名），没有另开连接。".to_owned());
        }
        Ok(crate::adapters::qualified(Engine::Sqlserver, &[schema, table]))
    }

    fn target_scope(&self, database: &str, schema: &str, table: &str) -> String {
        format!("{database} / {schema}.{table}")
    }

    fn rule_text(&self) -> &'static str {
        "这些语句逐条执行，失败即停。已成功的语句不撤销；这里也不把整批包进一个事务，不能据此声称可以整体回滚。"
    }

    /// T-SQL 的自增列是 IDENTITY(1, 1)。
    fn auto_clause_after_null(&self, draft: &ColumnDraft) -> &'static str {
        if draft.auto {
            " IDENTITY(1, 1)"
        } else {
            ""
        }
    }

    /// T-SQL 没有 ADD COLUMN 关键字，只有 ADD。
    fn alter_plan(&self, snapshot: &Snapshot, table: &str, _drafts: &[ColumnDraft], changes: &[Change], target: String, risks: Vec<Risk>) -> Plan {
        statement_alter_plan(snapshot, table, changes, target, risks, "ADD", &|qualified, original, draft| {
            Ok(modify_column(&snapshot.schema, table, qualified, original, draft))
        })
    }

    /// T-SQL 的索引名不带 schema 前缀，DROP INDEX 直接落在表上。
    fn drop_index_sql(&self, snapshot: &Snapshot, index: &str) -> Result<String, String> {
        Ok(format!("DROP INDEX {index} ON {};", self.qualified_table(&snapshot.database, &snapshot.schema, &snapshot.name)?))
    }

    fn references_cross_schema(&self) -> bool {
        true
    }

    fn reference_scope<'a>(&self, ref_schema: &'a str) -> Option<&'a str> {
        (!ref_schema.is_empty()).then_some(ref_schema)
    }

    fn truncate_note(&self) -> &'static str {
        "TRUNCATE TABLE 会重置 IDENTITY 计数，不触发 DELETE 触发器。"
    }

    fn rename_note(&self) -> Option<&'static str> {
        Some("sp_rename 不会改写引用旧名的存储过程、函数和视图。")
    }

    /// sp_rename 的新名字是裸名：加上方括号会变成名字的一部分。
    fn rename_table_sql(&self, snapshot: &Snapshot, table: &str, new_name: &str) -> Result<String, String> {
        let current = self.qualified_table(&snapshot.database, &snapshot.schema, table)?;
        Ok(format!("EXEC sp_rename N'{}', N'{}';", current.replace('\'', "''"), new_name.replace('\'', "''")))
    }

    fn rebuilt_missing(&self) -> &'static [&'static str] {
        &[
            "计算列表达式",
            "文件组与压缩选项",
            "主键的 CLUSTERED / NONCLUSTERED 选项（按 SQL Server 默认处理）",
            "索引选项（FILLFACTOR、IGNORE_DUP_KEY 等）",
            "XML 与空间索引",
            "权限",
            "触发器",
        ]
    }

    fn rebuilt_identity_missing(&self) -> Option<&'static str> {
        Some("IDENTITY 的起点与步长（统一写成 1, 1）")
    }

    fn rebuilt_auto_clause(&self) -> &'static str {
        " IDENTITY(1, 1)"
    }

    /// 外键没记目标模式时，目标表按本表所在模式限定。
    fn implied_reference_schema<'a>(&self, table_schema: &'a str) -> &'a str {
        table_schema
    }

    fn comment_statements(&self, snapshot: &Snapshot, _qualified: &str) -> Vec<String> {
        let literal = |text: &str| text.replace('\'', "''");
        let mut statements = Vec::new();
        let schema = literal(&snapshot.schema);
        let table = literal(&snapshot.name);
        if let Some(comment) = &snapshot.table_comment {
            statements.push(format!(
                "EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'{}', @level0type = N'SCHEMA', @level0name = N'{schema}', @level1type = N'TABLE', @level1name = N'{table}';",
                literal(comment)
            ));
        }
        for column in &snapshot.columns {
            if let Some(comment) = snapshot.column_comments.get(&column.name) {
                statements.push(format!(
                    "EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'{}', @level0type = N'SCHEMA', @level0name = N'{schema}', @level1type = N'TABLE', @level1name = N'{table}', @level2type = N'COLUMN', @level2name = N'{}';",
                    literal(comment),
                    literal(&column.name)
                ));
            }
        }
        statements
    }

    fn messages_en(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("TRUNCATE TABLE 会重置 IDENTITY 计数，不触发 DELETE 触发器。", "TRUNCATE TABLE resets the IDENTITY counter; DELETE triggers don't fire."),
            ("sp_rename 不会改写引用旧名的存储过程、函数和视图。", "sp_rename doesn't update stored procedures, functions or views that use the old name."),
        ]
    }

    fn missing_items_en(&self) -> &'static [(&'static str, &'static str)] {
        &[("IDENTITY 的起点与步长（统一写成 1, 1）", "the IDENTITY seed and increment (written as 1, 1)")]
    }

    fn matches_session(&self, session: &CatalogSession) -> bool {
        matches!(session, CatalogSession::Sqlserver(_))
    }

    async fn load_snapshot(&self, session: &CatalogSession, request: &Request, table: &str) -> Result<Snapshot, String> {
        let CatalogSession::Sqlserver(client) = session else {
            return Err(SESSION_MISMATCH.to_owned());
        };
        ddl_read::load_sqlserver(client, &request.database, &request.schema, table).await
    }

    async fn name_taken(&self, session: &CatalogSession, snapshot: &Snapshot, name: &str) -> Result<bool, String> {
        let CatalogSession::Sqlserver(client) = session else {
            return Err(SESSION_MISMATCH.to_owned());
        };
        ddl_read::name_taken(client, &snapshot.schema, &snapshot.name, name).await
    }

    async fn count(&self, session: &CatalogSession, sql: &str) -> Result<i64, String> {
        let CatalogSession::Sqlserver(client) = session else {
            return Err(SESSION_MISMATCH.to_owned());
        };
        ddl_read::count(client, sql).await
    }

    async fn load_comments(&self, session: &CatalogSession, schema: &str, table: &str) -> Option<Result<Comments, String>> {
        let CatalogSession::Sqlserver(client) = session else {
            return None;
        };
        Some(ddl_read::sqlserver_comments(client, schema, table).await)
    }
}

/// T-SQL 改一个字段：重命名走 sp_rename；类型/可空性走 ALTER COLUMN（不含内联 DEFAULT）；
/// 默认值是具名约束，走 DROP CONSTRAINT / ADD CONSTRAINT ... DEFAULT ... FOR。
fn modify_column(schema: &str, table: &str, qualified: &str, original: &Column, draft: &ColumnDraft) -> Vec<Step> {
    let mut steps = Vec::new();
    if draft.name.trim() != original.name {
        let rename_escaped = |value: &str| value.trim().replace('\'', "''");
        steps.push(Step {
            sql: format!(
                "EXEC sp_rename N'{}.{}.{}', N'{}', 'COLUMN';",
                rename_escaped(schema),
                rename_escaped(table),
                rename_escaped(&original.name),
                rename_escaped(draft.name.trim())
            ),
            label: format!("重命名 {} → {}", original.name, draft.name.trim()),
        });
    }
    let column_name = quote_ident(Engine::Sqlserver, draft.name.trim());
    let type_changed = !same_type(&draft.type_name, &original.type_name);
    if type_changed || draft.nullable != original.nullable {
        let mut definition = format!("{} {}", column_name, draft.type_name.trim());
        if !draft.nullable {
            definition.push_str(" NOT NULL");
        }
        steps.push(Step {
            sql: format!("ALTER TABLE {qualified} ALTER COLUMN {definition};"),
            label: format!("修改字段 {}", draft.name.trim()),
        });
    }
    if normalize_default(&draft.default_sql) != original.default_sql.as_deref().map(str::trim) {
        if draft.default_sql.trim().is_empty() {
            let sql = format!("{};", sqlserver_drop_default_sql(qualified, draft.name.trim(), original.default_constraint.as_deref().unwrap_or("")));
            steps.push(Step { sql, label: format!("删除默认值 {}", draft.name.trim()) });
        } else {
            // 既有默认值要先摘除：直接 ADD 同名约束报 2714、不同名报 1781，脚本必失败。
            // 与对比侧 tsql_default_change 同口径（有名走 DROP CONSTRAINT，无名按 sys.default_constraints 动态查找）。
            if original.default_sql.as_deref().map(str::trim).is_some_and(|value| !value.is_empty()) {
                let drop = format!("{};", sqlserver_drop_default_sql(qualified, original.name.trim(), original.default_constraint.as_deref().unwrap_or("")));
                steps.push(Step { sql: drop, label: format!("删除原默认值 {}", original.name) });
            }
            let table_name = sqlserver_bare_table_name(qualified);
            let sql = format!("{};", sqlserver_add_default_sql(qualified, &table_name, draft.name.trim(), draft.default_sql.trim()));
            steps.push(Step { sql, label: format!("修改默认值 {}", draft.name.trim()) });
        }
    }
    steps
}

/// SQL Server 的默认值是具名约束。删除时优先用读到的 sys.default_constraints 约束名；
/// 名字缺失时按系统视图动态查找后删除，找不到就不发语句（不猜 DF_ 前缀名）。
/// `table` 是已加引号的限定名（如 [dbo].[orders]），`column` 是裸列名。
pub(crate) fn sqlserver_drop_default_sql(table: &str, column: &str, constraint: &str) -> String {
    let name = constraint.trim();
    if !name.is_empty() {
        return format!("ALTER TABLE {table} DROP CONSTRAINT {}", quote_ident(Engine::Sqlserver, name));
    }
    let column_escaped = column.replace('\'', "''");
    let table_escaped = table.replace('\'', "''");
    format!(
        "DECLARE @dc sysname = (SELECT dc.name FROM sys.default_constraints dc JOIN sys.columns c ON c.object_id = dc.parent_object_id AND c.column_id = dc.parent_column_id WHERE dc.parent_object_id = OBJECT_ID(N'{table_escaped}') AND c.name = N'{column_escaped}'); IF @dc IS NOT NULL EXEC(N'ALTER TABLE {table_escaped} DROP CONSTRAINT ' + QUOTENAME(@dc))"
    )
}

/// 新增默认值：生成 DF_<表>_<列> 约束名（与 SQL Server 自动命名习惯一致，重名冲突由执行报错暴露）。
pub(crate) fn sqlserver_add_default_sql(table: &str, table_name: &str, column: &str, expression: &str) -> String {
    let constraint = format!("DF_{table_name}_{column}");
    format!(
        "ALTER TABLE {table} ADD CONSTRAINT {} DEFAULT {expression} FOR {}",
        quote_ident(Engine::Sqlserver, &constraint),
        quote_ident(Engine::Sqlserver, column)
    )
}

/// 从限定名 [dbo].[orders] 取末段裸表名，用于生成 DF_<表>_<列> 约束名。
pub(crate) fn sqlserver_bare_table_name(qualified: &str) -> String {
    qualified
        .rsplit('.')
        .next()
        .unwrap_or(qualified)
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned()
}

