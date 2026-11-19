//! SQL Server 的结构内省与执行：sys 视图读列/索引/外键，视图原文走 OBJECT_DEFINITION，逐条执行结构变更。

use std::collections::BTreeMap;

use crate::adapters::ddl::Comments;
use crate::adapters::quote_ident;
use crate::connections::model::Engine;
use crate::ddl::exec::receipt;
use crate::ddl::introspect::keep_rows;
use crate::ddl::model::{Column, ForeignKey, IncomingForeignKey, Index, IndexColumn, Snapshot, Step, StepReceipt, StepStatus};
use crate::ddl::sql::qualified_table;

/// 改名前查新名字：SQL Server 看同 schema 的 sys.objects（排除按原名逐字节相同的表自身）。
pub(super) async fn name_taken(client: &TdsHandle, schema: &str, current: &str, name: &str) -> Result<bool, String> {
    let rows = tds_strings(
        client,
        &format!(
            "SELECT COUNT(*) FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = N'{}' AND o.name = N'{}' AND NOT (o.name = N'{}' COLLATE Latin1_General_BIN2)",
            tds_escape(schema),
            tds_escape(name),
            tds_escape(current)
        ),
    )
    .await?;
    let text = rows.first().and_then(|row| row.first().cloned()).unwrap_or_default();
    Ok(text.trim().parse::<i64>().unwrap_or(0) > 0)
}

pub(super) async fn count(client: &TdsHandle, sql: &str) -> Result<i64, String> {
    let (_names, rows) = crate::adapters::sqlserver::grid_read::sqlserver_rows(client, sql).await?;
    let text = match rows.first().and_then(|row| row.first()) {
        Some(crate::grid::Cell::Text(text)) => text.clone(),
        _ => return Err("没有读到 NULL 计数".to_owned()),
    };
    text.parse::<i64>().map_err(|_| format!("不是整数 {text}"))
}

pub(super) async fn sqlserver_comments(client: &TdsHandle, schema: &str, table: &str) -> Result<Comments, String> {
    let rows = tds_strings(
        client,
        &format!(
            "SELECT COALESCE(c.name, N''), CAST(ep.value AS NVARCHAR(4000)) FROM sys.extended_properties ep JOIN sys.objects o ON o.object_id = ep.major_id JOIN sys.schemas s ON s.schema_id = o.schema_id LEFT JOIN sys.columns c ON c.object_id = ep.major_id AND c.column_id = ep.minor_id WHERE ep.class = 1 AND ep.name = N'MS_Description' AND s.name = N'{}' AND o.name = N'{}'",
            tds_escape(schema),
            tds_escape(table)
        ),
    )
    .await?;
    let mut table_comment = None;
    let mut columns = BTreeMap::new();
    for row in &rows {
        let column = row.first().cloned().unwrap_or_default();
        let comment = row.get(1).cloned().unwrap_or_default();
        if comment.is_empty() {
            continue;
        }
        if column.is_empty() {
            table_comment = Some(comment);
        } else {
            columns.insert(column, comment);
        }
    }
    Ok((table_comment, columns))
}

type TdsHandle = std::sync::Arc<tokio::sync::Mutex<crate::connections::drivers::TdsClient>>;

async fn tds_strings(client: &TdsHandle, sql: &str) -> Result<Vec<Vec<String>>, String> {
    let (_names, rows) = crate::adapters::sqlserver::grid_read::sqlserver_rows(client, sql).await?;
    Ok(rows
        .into_iter()
        .map(|cells| {
            cells
                .into_iter()
                .map(|cell| match cell {
                    crate::grid::Cell::Text(text) => text,
                    crate::grid::Cell::Null => String::new(),
                })
                .collect()
        })
        .collect())
}

/// SQL Server 索引里不在共用 Index 模型中的部分：索引类型（sys.indexes.type_desc）、INCLUDE 列与筛选条件。
#[derive(Clone, Debug, Default)]
pub(crate) struct SqlserverIndexExtra {
    pub name: String,
    pub type_desc: String,
    pub include: Vec<String>,
    pub filter: Option<String>,
}

/// 按读到的键列、聚集方式、INCLUDE 列和筛选条件拼出 SQL Server 的 CREATE INDEX 语句。
/// 聚集方式总是显式写出，避免默认值把聚集索引说成非聚集。列存储索引按各自语法输出；
/// XML、空间等其它类型读不到足够信息，只输出一行说明没有重建的注释，不编造语句。
pub(crate) fn sqlserver_index_sql(qualified: &str, index: &Index, extra: &SqlserverIndexExtra) -> String {
    let quote = |name: &str| quote_ident(Engine::Sqlserver, name);
    let name = quote(&index.name);
    let kind = extra.type_desc.trim().to_ascii_uppercase();
    let filter = extra.filter.as_deref().map(|filter| format!(" WHERE {filter}")).unwrap_or_default();
    match kind.as_str() {
        "CLUSTERED" | "NONCLUSTERED" => {}
        "CLUSTERED COLUMNSTORE" => return format!("CREATE CLUSTERED COLUMNSTORE INDEX {name} ON {qualified};"),
        "NONCLUSTERED COLUMNSTORE" => {
            let columns = index.columns.iter().map(|column| column.name.as_str()).chain(extra.include.iter().map(String::as_str)).map(quote).collect::<Vec<_>>().join(", ");
            return format!("CREATE NONCLUSTERED COLUMNSTORE INDEX {name} ON {qualified} ({columns}){filter};");
        }
        _ => return format!("-- 索引 {name} 的类型是 {}，没有从元数据重建。", if kind.is_empty() { "未知" } else { kind.as_str() }),
    }
    let keys = index
        .columns
        .iter()
        .map(|column| format!("{}{}", quote(&column.name), if column.descending { " DESC" } else { "" }))
        .collect::<Vec<_>>()
        .join(", ");
    let mut sql = format!("CREATE {}{kind} INDEX {name} ON {qualified} ({keys})", if index.unique { "UNIQUE " } else { "" });
    if !extra.include.is_empty() {
        let include = extra.include.iter().map(|name| quote_ident(Engine::Sqlserver, name)).collect::<Vec<_>>().join(", ");
        sql.push_str(&format!(" INCLUDE ({include})"));
    }
    sql.push_str(&filter);
    sql.push(';');
    sql
}

fn tds_escape(value: &str) -> String {
    value.replace('\'', "''")
}

/// SQL Server 的结构读取：sys 视图给出列/索引/外键，视图原文走 OBJECT_DEFINITION。
pub(super) async fn load_sqlserver(client: &TdsHandle, database: &str, schema: &str, table: &str) -> Result<Snapshot, String> {
    if database.is_empty() {
        return Err("需要数据库名，没有另开连接。".to_owned());
    }
    if schema.is_empty() {
        return Err("SQL Server 需要指定 schema（表所在的分组名），没有另开连接。".to_owned());
    }
    let (schema_q, table_q) = (tds_escape(schema), tds_escape(table));
    let kind_rows = tds_strings(
        client,
        &format!(
            "SELECT CASE WHEN o.type = 'V' THEN 'VIEW' ELSE 'BASE TABLE' END FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = N'{schema_q}' AND o.name = N'{table_q}' AND o.type IN ('U', 'V')"
        ),
    )
    .await?;
    let Some(kind_row) = kind_rows.first() else {
        return Err(format!("找不到表或视图 {schema}.{table}"));
    };
    let mut snapshot = Snapshot::bare(Engine::Sqlserver, database, schema, table);
    snapshot.kind = if kind_row.first().map(|text| text.eq_ignore_ascii_case("VIEW")).unwrap_or(false) { "view" } else { "table" }.to_owned();
    let columns = tds_strings(
        client,
        &format!(
            "SELECT c.name, TYPE_NAME(c.user_type_id) + CASE WHEN TYPE_NAME(c.system_type_id) IN ('varchar', 'nvarchar', 'varbinary') AND c.max_length = -1 THEN '(MAX)' ELSE '' END, c.is_nullable, c.is_identity, dc.definition, c.is_computed, dc.name FROM sys.columns c JOIN sys.objects o ON o.object_id = c.object_id JOIN sys.schemas s ON s.schema_id = o.schema_id LEFT JOIN sys.default_constraints dc ON dc.parent_object_id = c.object_id AND dc.parent_column_id = c.column_id WHERE s.name = N'{schema_q}' AND o.name = N'{table_q}' ORDER BY c.column_id"
        ),
    )
    .await
    .map_err(|err| err.to_string())?;
    for row in &columns {
        let cells: Vec<&String> = row.iter().collect();
        let flag = |index: usize| cells.get(index).map(|value| value.as_str() == "1");
        let text = |index: usize| cells.get(index).map(|value| (*value).clone()).unwrap_or_default();
        snapshot.columns.push(Column {
            name: text(0),
            type_name: text(1),
            nullable: flag(2).unwrap_or(true),
            default_sql: {
                let value = text(4);
                if value.is_empty() { None } else { Some(value) }
            },
            primary: false,
            auto: flag(3).unwrap_or(false),
            generated: flag(5).unwrap_or(false),
            // sys.default_constraints 里的具名默认约束；改/删默认值用它，不猜 DF_ 前缀名。
            default_constraint: {
                let name = text(6);
                if name.is_empty() { None } else { Some(name) }
            },
        });
    }
    let indexes: Vec<Vec<String>> = keep_rows(
        &mut snapshot.index_error,
        tds_strings(
            client,
            &format!(
                "SELECT i.name, i.is_primary_key, i.is_unique, ic.is_descending_key, c.name, ic.is_included_column, i.type_desc, i.has_filter, i.filter_definition FROM sys.indexes i JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id JOIN sys.objects o ON o.object_id = i.object_id JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = N'{schema_q}' AND o.name = N'{table_q}' AND i.name IS NOT NULL ORDER BY i.name, ic.is_included_column, ic.key_ordinal, ic.index_column_id"
            ),
        )
        .await,
    );
    // INCLUDE 列的 key_ordinal 是 0，不是键列：单独收集，只用于拼出完整的索引语句。
    let mut extras: Vec<SqlserverIndexExtra> = Vec::new();
    for row in &indexes {
        let cells: Vec<&String> = row.iter().collect();
        let flag = |index: usize| cells.get(index).map(|value| value.as_str() == "1");
        let text = |index: usize| cells.get(index).map(|value| (*value).clone()).unwrap_or_default();
        let name = text(0);
        let included = flag(5).unwrap_or(false);
        if !extras.iter().any(|extra| extra.name == name) {
            let filter = text(8).trim().to_owned();
            extras.push(SqlserverIndexExtra {
                name: name.clone(),
                type_desc: text(6).trim().to_owned(),
                include: Vec::new(),
                filter: (flag(7).unwrap_or(false) && !filter.is_empty()).then_some(filter),
            });
        }
        if included {
            if let Some(extra) = extras.iter_mut().find(|extra| extra.name == name) {
                extra.include.push(text(4));
            }
        }
        let index = match snapshot.indexes.iter_mut().position(|index| index.name == name) {
            Some(position) => &mut snapshot.indexes[position],
            None => {
                let primary = flag(1).unwrap_or(false);
                let unique = flag(2).unwrap_or(false);
                snapshot.indexes.push(Index {
                    name,
                    unique,
                    columns: Vec::new(),
                    protected: primary,
                    reason: if primary { "由主键约束维护".to_owned() } else { String::new() },
                    partial: false,
                    expression: false,
                    sql: None,
                });
                snapshot.indexes.last_mut().expect("just pushed")
            }
        };
        if !included {
            index.columns.push(IndexColumn { name: text(4), descending: flag(3).unwrap_or(false) });
        }
    }
    let qualified = qualified_table(Engine::Sqlserver, database, schema, table)?;
    for index in &mut snapshot.indexes {
        if index.protected {
            continue;
        }
        let Some(extra) = extras.iter().find(|extra| extra.name == index.name) else {
            continue;
        };
        index.partial = extra.filter.is_some();
        index.sql = Some(sqlserver_index_sql(&qualified, index, extra));
    }
    for column in &mut snapshot.columns {
        column.primary = snapshot
            .indexes
            .iter()
            .any(|index| index.protected && index.columns.iter().any(|item| item.name == column.name));
    }
    let foreign_keys: Vec<Vec<String>> = keep_rows(
        &mut snapshot.foreign_key_error,
        tds_strings(
            client,
            &format!(
                "SELECT fk.name, OBJECT_NAME(fk.parent_object_id), OBJECT_NAME(fk.referenced_object_id), fk.delete_referential_action_desc, fk.update_referential_action_desc FROM sys.foreign_keys fk JOIN sys.objects o ON o.object_id = fk.parent_object_id JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = N'{schema_q}' AND o.name = N'{table_q}'"
            ),
        )
        .await,
    );
    // FK 的列对来自 sys.foreign_key_columns；外键行与列行分开读，失败只记在 foreign_key_error。
    let fk_columns: Vec<Vec<String>> = keep_rows(
        &mut snapshot.foreign_key_error,
        tds_strings(
            client,
            &format!(
                "SELECT fk.name, pc.name, rc.name FROM sys.foreign_keys fk JOIN sys.objects o ON o.object_id = fk.parent_object_id JOIN sys.schemas s ON s.schema_id = o.schema_id JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id JOIN sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id WHERE s.name = N'{schema_q}' AND o.name = N'{table_q}' ORDER BY fk.name, fkc.constraint_column_id"
            ),
        )
        .await,
    );
    if snapshot.foreign_key_error.is_none() {
        let actions: std::collections::HashMap<String, (String, String)> = foreign_keys
            .iter()
            .map(|row| {
                (
                    row.first().cloned().unwrap_or_default(),
                    (
                        row.get(3).cloned().unwrap_or_else(|| "NO_ACTION".to_owned()),
                        row.get(4).cloned().unwrap_or_else(|| "NO_ACTION".to_owned()),
                    ),
                )
            })
            .collect();
        let ref_tables: std::collections::HashMap<String, String> = foreign_keys
            .iter()
            .map(|row| (row.first().cloned().unwrap_or_default(), row.get(2).cloned().unwrap_or_default()))
            .collect();
        for row in &fk_columns {
            let cells: Vec<&String> = row.iter().collect();
            let text = |index: usize| cells.get(index).map(|value| (*value).clone()).unwrap_or_default();
            let name = text(0);
            let column = text(1);
            let ref_column = text(2);
            if let Some(key) = snapshot.foreign_keys.iter_mut().find(|key| key.name == name) {
                key.columns.push(column);
                key.ref_columns.push(ref_column);
            } else {
                let (on_delete, on_update) = actions
                    .get(&name)
                    .map(|item| (tsql_fk_action(&item.0), tsql_fk_action(&item.1)))
                    .unwrap_or_else(|| ("NO ACTION".to_owned(), "NO ACTION".to_owned()));
                snapshot.foreign_keys.push(ForeignKey {
                    name: name.clone(),
                    columns: vec![column],
                    ref_schema: String::new(),
                    ref_table: ref_tables.get(&name).cloned().unwrap_or_default(),
                    ref_columns: vec![ref_column],
                    on_delete,
                    on_update,
                });
            }
        }
    }
    for key in &snapshot.foreign_keys {
        if let Some(index) = snapshot.indexes.iter_mut().find(|index| index.name == key.name) {
            index.protected = true;
            index.reason = format!("外键 {} 依赖此索引", key.name);
        }
    }
    let incoming: Vec<Vec<String>> = keep_rows(
        &mut snapshot.foreign_key_error,
        tds_strings(
            client,
            &format!(
                "SELECT fk.name, OBJECT_NAME(fk.parent_object_id), pc.name, rc.name FROM sys.foreign_keys fk JOIN sys.objects o ON o.object_id = fk.referenced_object_id JOIN sys.schemas s ON s.schema_id = o.schema_id JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id JOIN sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id WHERE s.name = N'{schema_q}' AND o.name = N'{table_q}' ORDER BY fk.name, fkc.constraint_column_id"
            ),
        )
        .await,
    );
    let mut incoming_keys: Vec<IncomingForeignKey> = Vec::new();
    for row in &incoming {
        let cells: Vec<&String> = row.iter().collect();
        let text = |index: usize| cells.get(index).map(|value| (*value).clone()).unwrap_or_default();
        let name = text(0);
        let child = text(1);
        let column = text(2);
        let referenced = text(3);
        if let Some(key) = incoming_keys.iter_mut().find(|key| key.name == name && key.table == child) {
            key.columns.push(column);
            key.ref_columns.push(referenced);
        } else {
            incoming_keys.push(IncomingForeignKey { name, table: child, schema: schema.to_owned(), columns: vec![column], ref_columns: vec![referenced] });
        }
    }
    snapshot.incoming = incoming_keys;
    snapshot.row_count = Some(tds_count(client, &qualified_table(Engine::Sqlserver, database, schema, table)?).await?);
    snapshot.object_exists = true;
    if snapshot.kind == "view" {
        match tds_view_definition(client, schema, table).await {
            Ok(sql) => snapshot.table_sql = sql,
            Err(message) => {
                snapshot.table_sql.clear();
                snapshot.definition_error = Some(message);
            }
        }
    } else {
        snapshot.table_sql = format!("-- {}.{table}", schema);
    }
    Ok(snapshot)
}

/// 估算行数：sys.partitions 的统计值；先用 TOP 1 确认是否为空，不在大表上跑 COUNT_BIG(*)。
async fn tds_count(client: &TdsHandle, qualified: &str) -> Result<i64, String> {
    let any = tds_strings(client, &format!("SELECT TOP 1 1 FROM {qualified}")).await?;
    if any.is_empty() {
        return Ok(0);
    }
    let literal = qualified.replace('\'', "''");
    let rows = tds_strings(
        client,
        &format!("SELECT CAST(ISNULL(SUM(p.rows), 0) AS bigint) FROM sys.partitions p WHERE p.object_id = OBJECT_ID(N'{literal}') AND p.index_id IN (0, 1)"),
    )
    .await?;
    let estimate = rows.first().and_then(|row| row.first()).and_then(|text| text.parse::<i64>().ok()).unwrap_or(0);
    Ok(estimate.max(1))
}

async fn tds_view_definition(client: &TdsHandle, schema: &str, table: &str) -> Result<String, String> {
    let rows = tds_strings(
        client,
        &format!(
            "SELECT OBJECT_DEFINITION(o.object_id) FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = N'{}' AND o.name = N'{}' AND o.type = 'V'",
            tds_escape(schema),
            tds_escape(table)
        ),
    )
    .await?;
    let text = rows.first().and_then(|row| row.first().cloned()).unwrap_or_default();
    if text.trim().is_empty() || text.trim().starts_with("--") {
        return Err("没有读到视图定义".to_owned());
    }
    Ok(text)
}

/// T-SQL 的引用动作描述（CASCADE / NO_ACTION / SET_NULL / SET_DEFAULT）映射回统一文案。
fn tsql_fk_action(value: &str) -> String {
    let normalized = value.to_ascii_uppercase().replace('_', " ");
    if normalized.is_empty() || normalized == "NO ACTION" {
        "NO ACTION".to_owned()
    } else {
        normalized
    }
}

pub(in crate::adapters) async fn run_steps(client: &std::sync::Arc<tokio::sync::Mutex<crate::connections::drivers::TdsClient>>, steps: &[Step]) -> Result<Vec<StepReceipt>, String> {
    let mut locked = client.lock().await;
    let mut results = Vec::new();
    let mut stop = false;
    for step in steps {
        if stop {
            results.push(receipt(step, StepStatus::Skipped, "前面的语句没有成功，这条没有执行。"));
            continue;
        }
        match locked.execute(step.sql.clone(), &[]).await {
            Ok(_) => results.push(receipt(step, StepStatus::Ok, "")),
            Err(tiberius::error::Error::Server(err)) => {
                stop = true;
                results.push(receipt(step, StepStatus::Failed, &crate::error_text::chain(&err)));
            }
            Err(err) => {
                stop = true;
                results.push(receipt(step, StepStatus::Unknown, &crate::error_text::chain(&err)));
            }
        }
    }
    Ok(results)
}
