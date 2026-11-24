//! SQL Server 在已打开会话上的目录读取：库与模式清单、分类分页、搜索与只读定义。
//! tiberius 句柄经互斥锁串行使用；别的库用三段名在同一条会话上读。

use crate::catalog::model::{known_row_estimate, parse_kind, DatabaseNode, ObjectKind, ObjectRow, SchemaNode};
use crate::catalog::read::{
    definition_failure, finish_kinds, read_failure, requested_limit, search_quota, unloaded_database, CatalogSearch, ObjectPageBody, ObjectWindow,
    SEARCH_PER_CONNECTION, SEARCH_PER_DATABASE,
};

use super::catalog::{kind_sql, listed_kinds, metadata_sql, sqlserver_qualified};

type TdsHandle = std::sync::Arc<tokio::sync::Mutex<crate::connections::drivers::TdsClient>>;

/// 执行带参数的查询并收齐所有行。分类查询都不大，一次性收完即可。
async fn tds_query_rows(client: &TdsHandle, sql: &str, params: &[&dyn tiberius::ToSql]) -> Result<Vec<tiberius::Row>, String> {
    use futures::TryStreamExt;
    let mut client = client.lock().await;
    let mut stream = client.query(sql, params).await.map_err(read_failure)?;
    let mut rows = Vec::new();
    while let Some(item) = stream.try_next().await.map_err(|err| read_failure(crate::error_text::chain(&err)))? {
        if let Some(row) = item.into_row() {
            rows.push(row);
        }
    }
    Ok(rows)
}

async fn tds_query_scalar(client: &TdsHandle, sql: &str, params: &[&dyn tiberius::ToSql]) -> Result<String, String> {
    let rows = tds_query_rows(client, sql, params).await?;
    let row = rows.into_iter().next().ok_or_else(|| read_failure("没有结果"))?;
    tds_text(&row, 0).ok_or_else(|| read_failure("元数据列是空值"))
}

fn tds_text(row: &tiberius::Row, index: usize) -> Option<String> {
    row.try_get::<&str, _>(index).ok().flatten().map(str::to_owned)
}

fn tds_i64(row: &tiberius::Row, index: usize) -> Option<i64> {
    if let Ok(Some(value)) = row.try_get::<i64, _>(index) {
        return Some(value);
    }
    if let Ok(Some(value)) = row.try_get::<i32, _>(index) {
        return Some(i64::from(value));
    }
    tds_text(row, index).and_then(|text| text.parse().ok())
}

pub(super) async fn sqlserver_current_database(client: &TdsHandle) -> Result<String, String> {
    tds_query_scalar(client, "SELECT DB_NAME()", &[]).await
}

fn assemble_sqlserver(names: &[String], current: &str, schemas: Result<Vec<SchemaNode>, String>) -> Vec<DatabaseNode> {
    names
        .iter()
        .map(|name| {
            if name == current {
                match &schemas {
                    Ok(nodes) => DatabaseNode {
                        name: name.clone(),
                        schemas: nodes.clone(),
                        error: None,
                    },
                    Err(message) => DatabaseNode {
                        name: name.clone(),
                        schemas: Vec::new(),
                        error: Some(message.clone()),
                    },
                }
            } else {
                unloaded_database(name)
            }
        })
        .collect()
}

fn schemas_sql() -> Result<&'static str, String> {
    metadata_sql().schemas.ok_or_else(|| read_failure("SQL Server 缺少 schema 查询"))
}

/// 单个库的 schema 清单（不含对象）：在原会话上用三段名读。
pub(super) async fn read_database_schemas(client: &TdsHandle, database: &str) -> Result<Vec<SchemaNode>, String> {
    let rows = tds_query_rows(client, &sqlserver_qualified(schemas_sql()?, database), &[]).await?;
    let mut schemas = Vec::new();
    for row in &rows {
        schemas.push(SchemaNode::empty(tds_text(row, 0).ok_or_else(|| read_failure("元数据列是空值"))?));
    }
    Ok(schemas)
}

async fn sqlserver_database_names(client: &TdsHandle) -> Result<(Vec<String>, String), String> {
    let sql = metadata_sql();
    let rows = tds_query_rows(client, sql.databases, &[]).await?;
    let mut names = Vec::new();
    for row in &rows {
        names.push(tds_text(row, 0).ok_or_else(|| read_failure("元数据列是空值"))?);
    }
    let current = sqlserver_current_database(client).await?;
    if !names.iter().any(|name| name == &current) {
        names.push(current.clone());
        names.sort();
    }
    Ok((names, current))
}

pub(super) async fn read_sqlserver(client: &TdsHandle, page: usize, windows: &[ObjectWindow]) -> Result<Vec<DatabaseNode>, String> {
    let (names, current) = sqlserver_database_names(client).await?;
    let schemas_sql = schemas_sql()?;
    let schema_result = match tds_query_rows(client, schemas_sql, &[]).await {
        Ok(rows) => {
            let mut schemas = Vec::new();
            for row in &rows {
                let name = tds_text(row, 0).ok_or_else(|| read_failure("元数据列是空值"))?;
                let limit = requested_limit(&current, &name, windows, page);
                schemas.push(read_sqlserver_schema(client, &current, &name, 0, limit).await);
            }
            Ok(schemas)
        }
        Err(message) => Err(message),
    };
    Ok(assemble_sqlserver(&names, &current, schema_result))
}

pub(super) async fn read_sqlserver_outline(client: &TdsHandle) -> Result<Vec<DatabaseNode>, String> {
    let (names, current) = sqlserver_database_names(client).await?;
    let schemas_sql = schemas_sql()?;
    let schema_result = match tds_query_rows(client, schemas_sql, &[]).await {
        Ok(rows) => {
            let mut schemas = Vec::new();
            for row in &rows {
                schemas.push(SchemaNode::empty(tds_text(row, 0).ok_or_else(|| read_failure("元数据列是空值"))?));
            }
            Ok(schemas)
        }
        Err(message) => Err(message),
    };
    Ok(assemble_sqlserver(&names, &current, schema_result))
}

pub(super) async fn read_sqlserver_page(client: &TdsHandle, database: &str, schema: &str, offset: usize, limit: usize) -> ObjectPageBody {
    // 别的库同样在这条会话上读：三段名限定目录视图，不发 USE，也不改动当前库。
    let schema_node = read_sqlserver_schema(client, database, schema, offset, limit).await;
    ObjectPageBody {
        objects: schema_node.objects,
        total: schema_node.total,
        error: schema_node.error,
        kind_reports: schema_node.kind_reports,
    }
}

async fn read_sqlserver_schema(client: &TdsHandle, database: &str, schema: &str, offset: usize, limit: usize) -> SchemaNode {
    let window = offset.saturating_add(limit);
    let mut parts = Vec::new();
    for kind in listed_kinds() {
        parts.push((*kind, read_sqlserver_kind(client, database, schema, *kind, window).await));
    }
    let (objects, total, reports) = finish_kinds(parts, offset, limit);
    SchemaNode {
        name: schema.to_owned(),
        objects,
        total,
        error: None,
        kind_reports: reports,
    }
}

async fn read_sqlserver_kind(client: &TdsHandle, database: &str, schema: &str, kind: ObjectKind, window: usize) -> Result<(usize, Vec<ObjectRow>), String> {
    read_sqlserver_kind_slice(client, database, schema, kind, 0, window).await
}

pub(super) async fn read_sqlserver_kind_slice(
    client: &TdsHandle,
    database: &str,
    schema: &str,
    kind: ObjectKind,
    offset: usize,
    window: usize,
) -> Result<(usize, Vec<ObjectRow>), String> {
    let sql = kind_sql(kind);
    let count_rows = tds_query_rows(client, &sqlserver_qualified(sql.count, database), &[&schema]).await?;
    let total = tds_i64(count_rows.first().ok_or_else(|| read_failure("没有对象数量"))?, 0).unwrap_or(0) as usize;
    let window_i = i64::try_from(window).unwrap_or(i64::MAX);
    let offset_i = i64::try_from(offset).unwrap_or(i64::MAX);
    let page_sql = sqlserver_qualified(&sql.list.replace("OFFSET 0 ROWS", "OFFSET @P3 ROWS"), database);
    let rows = tds_query_rows(client, &page_sql, &[&schema, &window_i, &offset_i]).await?;
    let mut objects = Vec::new();
    for row in &rows {
        objects.push(sqlserver_object(row)?);
    }
    Ok((total, objects))
}

fn sqlserver_object(row: &tiberius::Row) -> Result<ObjectRow, String> {
    let name = tds_text(row, 0).ok_or_else(|| read_failure("元数据列是空值"))?;
    let kind = tds_text(row, 1).ok_or_else(|| read_failure("元数据列是空值"))?;
    Ok(ObjectRow {
        name,
        kind: parse_kind(&kind),
        estimated_rows: known_row_estimate(tds_i64(row, 2)),
        stale: false,
        signature: tds_text(row, 3).unwrap_or_default(),
        relation: tds_text(row, 4).unwrap_or_default(),
        materialized: false,
    })
}

fn sqlserver_bracket(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

/// 一个库里名字命中的用户对象；`select` 是 SELECT 列表，库名用三段名限定，在同一条会话上读别的库。
fn sqlserver_search_sql(database: &str, pattern: &str, select: &str, tail: &str) -> String {
    let db = sqlserver_bracket(database);
    let escaped = pattern.replace('\'', "''");
    format!(
        "SELECT {select} FROM {db}.sys.objects o JOIN {db}.sys.schemas s ON s.schema_id = o.schema_id \
         WHERE o.is_ms_shipped = 0 AND o.type IN ('U', 'V', 'P', 'FN', 'IF', 'TF', 'TR', 'SO') \
           AND LOWER(o.name) LIKE LOWER(N'{escaped}') ESCAPE '\\' {tail}"
    )
}

const SQLSERVER_SEARCH_COLUMNS: &str = "s.name, o.name, \
    CASE o.type WHEN 'V' THEN 'view' WHEN 'P' THEN 'procedure' WHEN 'SO' THEN 'sequence' WHEN 'TR' THEN 'trigger' \
    WHEN 'FN' THEN 'function' WHEN 'IF' THEN 'function' WHEN 'TF' THEN 'function' ELSE 'table' END";

fn cell_text(cell: Option<&crate::grid::Cell>) -> Option<String> {
    match cell? {
        crate::grid::Cell::Text(text) => Some(text.clone()),
        crate::grid::Cell::Null => None,
    }
}

async fn sqlserver_search_count(client: &TdsHandle, database: &str, pattern: &str) -> Result<usize, String> {
    let sql = sqlserver_search_sql(database, pattern, "COUNT(*)", "");
    let (_, rows) = crate::adapters::sqlserver::grid_read::sqlserver_rows(client, &sql).await.map_err(read_failure)?;
    Ok(rows.first().and_then(|row| cell_text(row.first())).and_then(|text| text.trim().parse().ok()).unwrap_or(0))
}

async fn sqlserver_search_fetch(client: &TdsHandle, database: &str, pattern: &str, offset: usize, take: usize, result: &mut CatalogSearch) -> Result<(), String> {
    let tail = format!("ORDER BY s.name, o.name OFFSET {offset} ROWS FETCH NEXT {take} ROWS ONLY");
    let sql = sqlserver_search_sql(database, pattern, SQLSERVER_SEARCH_COLUMNS, &tail);
    let (_, rows) = crate::adapters::sqlserver::grid_read::sqlserver_rows(client, &sql).await.map_err(read_failure)?;
    result.push_page(database, rows, |row| {
        let (Some(schema), Some(name)) = (cell_text(row.first()), cell_text(row.get(1))) else {
            return None;
        };
        let kind = cell_text(row.get(2)).unwrap_or_else(|| "table".to_owned());
        Some((schema, ObjectRow::fresh(name, parse_kind(&kind))))
    });
    Ok(())
}

/// 资源管理器全局搜索：当前库和同一连接上可访问的其他用户库，按库分配名额，失败按库报告。
pub(super) async fn search(client: &TdsHandle, pattern: &str) -> CatalogSearch {
    let current = match sqlserver_current_database(client).await {
        Ok(db) => db,
        Err(message) => return CatalogSearch::failed("", message),
    };
    let list = "SELECT name FROM sys.databases WHERE state = 0 AND (database_id > 4 OR name = DB_NAME()) AND HAS_DBACCESS(name) = 1 ORDER BY name";
    let mut result = CatalogSearch::default();
    let databases = match crate::adapters::sqlserver::grid_read::sqlserver_rows(client, list).await {
        Ok((_, rows)) => rows.iter().filter_map(|row| cell_text(row.first())).collect::<Vec<_>>(),
        Err(error) => {
            // 读不到库清单时至少搜当前库，并说明原因。
            result.failures.push((String::new(), read_failure(error)));
            vec![current.clone()]
        }
    };
    let mut counts = Vec::new();
    for database in &databases {
        match sqlserver_search_count(client, database, pattern).await {
            Ok(0) => {}
            Ok(count) => counts.push((database.clone(), count)),
            Err(message) => result.failures.push((database.clone(), message)),
        }
    }
    for ((database, count), (_, take)) in counts.iter().zip(search_quota(&counts, SEARCH_PER_DATABASE, SEARCH_PER_CONNECTION)) {
        if take > 0 {
            if let Err(message) = sqlserver_search_fetch(client, database, pattern, 0, take, &mut result).await {
                result.failures.push((database.clone(), message));
                continue;
            }
        }
        if take < *count {
            result.truncated.push((database.clone(), Some(count - take)));
        }
    }
    result
}

/// 展开截断的库：按名字顺序从 `offset` 起再取 `limit` 条，并给出之后还剩多少。
pub(super) async fn search_database(client: &TdsHandle, database: &str, pattern: &str, offset: usize, limit: usize) -> CatalogSearch {
    let total = match sqlserver_search_count(client, database, pattern).await {
        Ok(total) => total,
        Err(message) => return CatalogSearch::failed(database, message),
    };
    let mut result = CatalogSearch::default();
    if offset >= total {
        return result;
    }
    let take = limit.min(total - offset);
    if let Err(message) = sqlserver_search_fetch(client, database, pattern, offset, take, &mut result).await {
        return CatalogSearch::failed(database, message);
    }
    let remaining = total - offset - take;
    if remaining > 0 {
        result.truncated.push((database.to_owned(), Some(remaining)));
    }
    result
}

/// SQL Server 的只读定义：函数/过程/触发器/视图走 OBJECT_DEFINITION，序列按 sys.sequences 重建。
/// sys.sequences 的定义读取：start/increment/min/max 是 sql_variant、is_* 是位类型，
/// 都在 SQL 侧投影成文本/0-1；读不到就报错，不静默编造 START 1 / NO CYCLE 之类的默认值。
pub(crate) const SQLSERVER_SEQUENCE_SQL: &str = "SELECT TYPE_NAME(q.user_type_id), CAST(q.start_value AS NVARCHAR(40)), CAST(q.increment AS NVARCHAR(40)), CAST(q.minimum_value AS NVARCHAR(40)), CAST(q.maximum_value AS NVARCHAR(40)), CASE WHEN q.is_cycling THEN N'1' ELSE N'0' END, CASE WHEN q.is_cached THEN N'1' ELSE N'0' END, CAST(q.cache_size AS NVARCHAR(40)) FROM sys.sequences q JOIN sys.schemas s ON s.schema_id = q.schema_id WHERE s.name = @P1 AND q.name = @P2";

pub(super) async fn read_sqlserver_definition(client: &TdsHandle, schema: &str, name: &str, kind: &str) -> Result<String, String> {
    if schema.is_empty() {
        return Err(definition_failure("SQL Server 需要 schema"));
    }
    match kind {
        "function" | "procedure" | "trigger" | "view" => {
            let sql = "SELECT OBJECT_DEFINITION(o.object_id) FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE s.name = @P1 AND o.name = @P2";
            let rows = tds_query_rows(client, sql, &[&schema, &name]).await.map_err(definition_failure)?;
            if rows.len() > 1 {
                return Err(definition_failure("有多个同名对象，需要更具体的名称"));
            }
            let row = rows.first().ok_or_else(|| definition_failure("没有读到定义"))?;
            let text = tds_text(row, 0).ok_or_else(|| definition_failure("没有读到定义"))?;
            if text.trim().is_empty() {
                return Err(definition_failure("没有读到定义"));
            }
            Ok(text)
        }
        "sequence" => {
            let sql = SQLSERVER_SEQUENCE_SQL;
            let rows = tds_query_rows(client, sql, &[&schema, &name]).await.map_err(definition_failure)?;
            let row = rows.first().ok_or_else(|| definition_failure("没有读到定义"))?;
            let type_name = tds_text(row, 0).ok_or_else(|| definition_failure("没有读到定义"))?;
            let start = tds_text(row, 1).ok_or_else(|| definition_failure("没有读到序列起点"))?;
            let increment = tds_text(row, 2).ok_or_else(|| definition_failure("没有读到序列步长"))?;
            let minimum = tds_text(row, 3).ok_or_else(|| definition_failure("没有读到序列下限"))?;
            let maximum = tds_text(row, 4).ok_or_else(|| definition_failure("没有读到序列上限"))?;
            let cycling = tds_text(row, 5).map(|value| value == "1").unwrap_or(false);
            let cached = tds_text(row, 6).map(|value| value == "1").unwrap_or(false);
            let cache_size = tds_text(row, 7).unwrap_or_default();
            let cycle_text = if cycling { " CYCLE" } else { " NO CYCLE" };
            let cache_text = if cached && !cache_size.is_empty() {
                format!(" CACHE {cache_size}")
            } else if cached {
                " CACHE".to_owned()
            } else {
                " NO CACHE".to_owned()
            };
            let bounds = format!(
                "{}{}",
                if minimum.is_empty() { String::new() } else { format!(" MINVALUE {minimum}") },
                if maximum.is_empty() { String::new() } else { format!(" MAXVALUE {maximum}") },
            );
            Ok(format!(
                "CREATE SEQUENCE [{schema}].[{name}] AS {type_name} START WITH {start} INCREMENT BY {increment}{bounds}{cycle_text}{cache_text};"
            ))
        }
        "enum" | "domain" => Err(definition_failure("SQL Server 没有独立的枚举或域")),
        _ => Err(definition_failure("只有函数、存储过程、触发器、视图和序列可以读取只读定义")),
    }
}

/// 在目录会话上执行一条只读语句，把每格转成文本（对象详情与补全外键读取共用）。
pub(in crate::adapters) async fn text_rows(client: &TdsHandle, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
    use crate::catalog::detail::failure;
    use futures::TryStreamExt;
    let mut client = client.lock().await;
    let mut stream = client.query(sql, &[]).await.map_err(failure)?;
    let mut rows = Vec::new();
    while let Some(item) = stream.try_next().await.map_err(|err| failure(err.to_string()))? {
        let Some(row) = item.into_row() else { continue };
        let width = row.columns().len();
        rows.push(
            (0..width)
                .map(|index| {
                    if let Ok(value) = row.try_get::<&str, _>(index) {
                        return value.map(str::to_owned);
                    }
                    if let Ok(value) = row.try_get::<i32, _>(index) {
                        return value.map(|number| number.to_string());
                    }
                    if let Ok(value) = row.try_get::<bool, _>(index) {
                        return value.map(|flag| if flag { "1".to_owned() } else { "0".to_owned() });
                    }
                    None
                })
                .collect::<Vec<Option<String>>>(),
        );
    }
    Ok(rows)
}

#[cfg(test)]
mod database_scope_tests {
    use super::*;
    use crate::catalog::read::read_database_outline;
    use crate::connections::model::Engine;
    use crate::connections::CatalogSession;

    /// 按库读 SQL Server 时，目录视图与只在当前库生效的函数都要换成三段名；
    /// 空库名（当前库路径）保持原样。
    #[test]
    fn sqlserver_sql_is_scoped_with_three_part_names() {
        let schemas = metadata_sql().schemas.unwrap();
        let scoped = sqlserver_qualified(schemas, "audit");
        assert!(scoped.contains("[audit].sys.schemas"), "{scoped}");
        assert!(!scoped.contains("FROM sys.schemas"), "{scoped}");

        let trigger = kind_sql(ObjectKind::Trigger).list;
        let scoped = sqlserver_qualified(trigger, "au]dit");
        assert!(scoped.contains("[au]]dit].sys.triggers"), "方括号要按 SQL 规则转义：{scoped}");
        assert!(!scoped.contains("OBJECT_NAME(t.parent_id)"), "父对象名要按目标库查：{scoped}");
        assert!(scoped.contains("(SELECT name FROM [au]]dit].sys.objects WHERE object_id = t.parent_id)"), "{scoped}");

        assert_eq!(sqlserver_qualified(trigger, "  "), trigger, "当前库不加限定");
    }

    /// 按库读取必须有库名；不分库的引擎不需要这条路径。
    #[tokio::test]
    async fn a_database_scope_read_needs_a_database_on_a_layered_engine() {
        let missing = read_database_outline(Engine::Postgres, &CatalogSession::Scripted, "  ").await.unwrap_err();
        assert!(missing.contains("数据库名"), "{missing}");
        let flat = read_database_outline(Engine::Sqlite, &CatalogSession::Scripted, "main").await.unwrap_err();
        assert!(flat.contains("不需要按库再读"), "{flat}");
    }
}
