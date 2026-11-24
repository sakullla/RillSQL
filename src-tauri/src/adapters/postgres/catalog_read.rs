//! PostgreSQL 在已打开会话上的目录读取：库与模式清单、分类分页、搜索与只读定义。

use crate::catalog::model::{known_row_estimate, parse_kind, DatabaseNode, ObjectKind, ObjectRow, SchemaNode};
use crate::catalog::read::{
    assemble_postgres, definition_failure, empty_page, finish_kinds, other_database_message, read_failure, requested_limit, CatalogSearch,
    ObjectPageBody, ObjectWindow, SEARCH_PER_DATABASE,
};

use super::catalog::{kind_sql, listed_kinds, metadata_sql};

fn schemas_sql() -> Result<&'static str, String> {
    metadata_sql().schemas.ok_or_else(|| read_failure("PostgreSQL 缺少 schema 查询"))
}

pub(super) async fn read_postgres(client: &tokio_postgres::Client, page: usize, windows: &[ObjectWindow]) -> Result<Vec<DatabaseNode>, String> {
    let (names, current) = postgres_database_names(client).await?;
    let schemas_sql = schemas_sql()?;
    let schema_result = match client.query(schemas_sql, &[]).await {
        Ok(rows) => {
            let mut schemas = Vec::new();
            for row in &rows {
                let name = pg_string(row, 0)?;
                let limit = requested_limit(&current, &name, windows, page);
                schemas.push(read_postgres_schema(client, &name, 0, limit).await);
            }
            Ok(schemas)
        }
        Err(err) => Err(read_failure(err)),
    };
    Ok(assemble_postgres(&names, &current, schema_result))
}

pub(super) async fn read_postgres_outline(client: &tokio_postgres::Client) -> Result<Vec<DatabaseNode>, String> {
    let (names, current) = postgres_database_names(client).await?;
    let schemas_sql = schemas_sql()?;
    let schema_result = match client.query(schemas_sql, &[]).await {
        Ok(rows) => {
            let mut schemas = Vec::new();
            for row in &rows {
                schemas.push(SchemaNode::empty(pg_string(row, 0)?));
            }
            Ok(schemas)
        }
        Err(err) => Err(read_failure(err)),
    };
    Ok(assemble_postgres(&names, &current, schema_result))
}

/// 单个库的 schema 清单（不含对象）：会话必须已经连到目标库。
pub(super) async fn read_database_schemas(client: &tokio_postgres::Client, database: &str) -> Result<Vec<SchemaNode>, String> {
    let current = current_database(client).await?;
    if current != database {
        return Err(read_failure(format!("临时读取会话连到了 {current}，不是 {database}")));
    }
    let rows = client.query(schemas_sql()?, &[]).await.map_err(read_failure)?;
    let mut schemas = Vec::new();
    for row in &rows {
        schemas.push(SchemaNode::empty(pg_string(row, 0)?));
    }
    Ok(schemas)
}

async fn postgres_database_names(client: &tokio_postgres::Client) -> Result<(Vec<String>, String), String> {
    let sql = metadata_sql();
    let db_rows = client.query(sql.databases, &[]).await.map_err(read_failure)?;
    let mut names = Vec::new();
    for row in &db_rows {
        names.push(pg_string(row, 0)?);
    }
    let current = current_database(client).await?;
    if !names.iter().any(|name| name == &current) {
        names.push(current.clone());
        names.sort();
    }
    Ok((names, current))
}

pub(super) async fn read_postgres_page(client: &tokio_postgres::Client, database: &str, schema: &str, offset: usize, limit: usize) -> ObjectPageBody {
    let current = match current_database(client).await {
        Ok(name) => name,
        Err(message) => return empty_page(message),
    };
    if database != current {
        return empty_page(other_database_message(&current, database));
    }
    let schema_node = read_postgres_schema(client, schema, offset, limit).await;
    ObjectPageBody {
        objects: schema_node.objects,
        total: schema_node.total,
        error: schema_node.error,
        kind_reports: schema_node.kind_reports,
    }
}

pub(super) async fn current_database(client: &tokio_postgres::Client) -> Result<String, String> {
    let rows = client.query("SELECT current_database()", &[]).await.map_err(read_failure)?;
    let row = rows.first().ok_or_else(|| read_failure("没有当前数据库"))?;
    pg_string(row, 0)
}

async fn read_postgres_schema(client: &tokio_postgres::Client, schema: &str, offset: usize, limit: usize) -> SchemaNode {
    let window = offset.saturating_add(limit);
    let mut parts = Vec::new();
    for kind in listed_kinds() {
        parts.push((*kind, read_postgres_kind(client, schema, *kind, window).await));
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

async fn read_postgres_kind(client: &tokio_postgres::Client, schema: &str, kind: ObjectKind, window: usize) -> Result<(usize, Vec<ObjectRow>), String> {
    read_postgres_kind_slice(client, schema, kind, 0, window).await
}

pub(super) async fn read_postgres_kind_slice(
    client: &tokio_postgres::Client,
    schema: &str,
    kind: ObjectKind,
    offset: usize,
    window: usize,
) -> Result<(usize, Vec<ObjectRow>), String> {
    let sql = kind_sql(kind);
    let count_params: [&(dyn tokio_postgres::types::ToSql + Sync); 1] = [&schema];
    let count_rows = client.query(sql.count, &count_params).await.map_err(read_failure)?;
    let total = match count_rows.first().map(|row| pg_i64(row, 0)) {
        Some(Ok(value)) => usize::try_from(value).map_err(|_| read_failure("对象数量无效"))?,
        Some(Err(message)) => return Err(message),
        None => return Err(read_failure("没有对象数量")),
    };
    let window_i = i64::try_from(window).unwrap_or(i64::MAX);
    let offset_i = i64::try_from(offset).unwrap_or(i64::MAX);
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 3] = [&schema, &window_i, &offset_i];
    let page_sql = format!("{} OFFSET $3", sql.list);
    let rows = client.query(&page_sql, &params).await.map_err(read_failure)?;
    let mut objects = Vec::new();
    for row in &rows {
        objects.push(postgres_object(row)?);
    }
    Ok((total, objects))
}

const POSTGRES_SEARCH_FROM: &str = "FROM pg_catalog.pg_class c \
    JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
    WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') \
      AND n.nspname NOT LIKE 'pg_toast%' \
      AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S') \
      AND c.relname ILIKE $1 ESCAPE '\\'";

/// 资源管理器全局搜索：当前库里所有用户模式的表、视图、物化视图、外部表与序列。
/// 会话只能读自己所在的库：同一连接的其他库列在 `unsearched` 里由界面说明，不假装搜过。
pub(super) async fn search(client: &tokio_postgres::Client, pattern: &str) -> CatalogSearch {
    let db = match current_database(client).await {
        Ok(db) => db,
        Err(message) => return CatalogSearch::failed("", message),
    };
    let mut result = search_database(client, &db, pattern, 0, SEARCH_PER_DATABASE).await;
    let others = "SELECT datname FROM pg_catalog.pg_database WHERE datallowconn AND NOT datistemplate AND datname <> current_database() ORDER BY datname";
    // 读不到库清单不影响当前库的命中；只是无法说明还有哪些库没搜。
    if let Ok(rows) = client.query(others, &[]).await {
        result.unsearched = rows.iter().filter_map(|row| row.try_get::<_, String>(0).ok()).collect();
    }
    result
}

/// 当前库里按名字顺序从 `offset` 起取 `limit` 条命中，并报告之后还剩多少。别的库读不到，直接说明原因。
pub(super) async fn search_database(client: &tokio_postgres::Client, database: &str, pattern: &str, offset: usize, limit: usize) -> CatalogSearch {
    let current = match current_database(client).await {
        Ok(db) => db,
        Err(message) => return CatalogSearch::failed(database, message),
    };
    if current != database {
        return CatalogSearch::failed(database, read_failure(other_database_message(&current, database)));
    }
    let total = match client.query_one(&format!("SELECT COUNT(*) {POSTGRES_SEARCH_FROM}"), &[&pattern]).await {
        Ok(row) => usize::try_from(row.get::<_, i64>(0)).unwrap_or(0),
        Err(error) => return CatalogSearch::failed(database, read_failure(error)),
    };
    let mut result = CatalogSearch::default();
    if offset >= total {
        return result;
    }
    let take = limit.min(total - offset);
    let sql = format!(
        "SELECT n.nspname, c.relname, \
         CASE c.relkind WHEN 'v' THEN 'view' WHEN 'm' THEN 'view' WHEN 'S' THEN 'sequence' ELSE 'table' END, \
         (c.relkind = 'm') {POSTGRES_SEARCH_FROM} ORDER BY n.nspname, c.relname LIMIT {take} OFFSET {offset}"
    );
    let rows = match client.query(&sql, &[&pattern]).await {
        Ok(rows) => rows,
        Err(error) => return CatalogSearch::failed(database, read_failure(error)),
    };
    // 解不出的行记在 skipped 里：它们也占了 OFFSET 窗口，展开下一页时要算上。
    result.push_page(database, rows, |row| {
        let (Ok(schema), Ok(name), Ok(kind), Ok(materialized)) =
            (row.try_get::<_, String>(0), row.try_get::<_, String>(1), row.try_get::<_, String>(2), row.try_get::<_, bool>(3))
        else {
            return None;
        };
        let mut object = ObjectRow::fresh(name, parse_kind(&kind));
        object.materialized = materialized;
        Some((schema, object))
    });
    let remaining = total - offset - take;
    if remaining > 0 {
        result.truncated.push((database.to_owned(), Some(remaining)));
    }
    result
}

pub(super) async fn read_postgres_definition(
    client: &tokio_postgres::Client,
    schema: &str,
    name: &str,
    kind: &str,
    signature: &str,
    relation: &str,
) -> Result<String, String> {
    if schema.is_empty() {
        return Err(definition_failure("PostgreSQL 需要 schema"));
    }
    match kind {
        "function" | "procedure" => {
            let prokind = if kind == "procedure" { "p" } else { "f" };
            let sql = "SELECT pg_get_functiondef(p.oid) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1 AND p.proname = $2 AND p.prokind = $3 AND ($4 = '' OR pg_get_function_identity_arguments(p.oid) = $4)";
            let rows = client.query(sql, &[&schema, &name, &prokind, &signature]).await.map_err(definition_failure)?;
            if rows.len() > 1 {
                return Err(definition_failure("有多个同名函数，需要用参数类型区分"));
            }
            let row = rows.first().ok_or_else(|| definition_failure("没有读到定义"))?;
            let text = pg_string(row, 0).map_err(definition_failure)?;
            if text.trim().is_empty() {
                return Err(definition_failure("没有读到定义"));
            }
            Ok(text)
        }
        "sequence" => read_postgres_text(
            client,
            "SELECT format('CREATE SEQUENCE %I.%I AS %s INCREMENT BY %s MINVALUE %s MAXVALUE %s START WITH %s CACHE %s %s;', schemaname, sequencename, data_type, increment_by, min_value, max_value, start_value, cache_size, CASE WHEN cycle THEN 'CYCLE' ELSE 'NO CYCLE' END) FROM pg_catalog.pg_sequences WHERE schemaname = $1 AND sequencename = $2",
            schema,
            name,
        )
        .await,
        "enum" => read_postgres_text(
            client,
            "SELECT format('CREATE TYPE %I.%I AS ENUM (%s);', n.nspname, t.typname, COALESCE((SELECT string_agg(quote_literal(e.enumlabel), ', ' ORDER BY e.enumsortorder) FROM pg_enum e WHERE e.enumtypid = t.oid), '')) FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE n.nspname = $1 AND t.typname = $2 AND t.typtype = 'e'",
            schema,
            name,
        )
        .await,
        "domain" => read_postgres_text(
            client,
            "SELECT format('CREATE DOMAIN %I.%I AS %s%s%s%s;', n.nspname, t.typname, pg_catalog.format_type(t.typbasetype, t.typtypmod), CASE WHEN t.typnotnull THEN ' NOT NULL' ELSE '' END, CASE WHEN t.typdefault IS NOT NULL THEN ' DEFAULT ' || t.typdefault ELSE '' END, COALESCE((SELECT ' ' || string_agg(pg_get_constraintdef(c.oid), ' ' ORDER BY c.oid) FROM pg_constraint c WHERE c.contypid = t.oid), '')) FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE n.nspname = $1 AND t.typname = $2 AND t.typtype = 'd'",
            schema,
            name,
        )
        .await,
        "trigger" => {
            let sql = "SELECT pg_get_triggerdef(t.oid, true) FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND t.tgname = $2 AND NOT t.tgisinternal AND ($3 = '' OR c.relname = $3)";
            let rows = client.query(sql, &[&schema, &name, &relation]).await.map_err(definition_failure)?;
            if rows.len() > 1 {
                return Err(definition_failure("有多个同名触发器，需要所属的表才能区分"));
            }
            let row = rows.first().ok_or_else(|| definition_failure("没有读到定义"))?;
            let text = pg_string(row, 0).map_err(definition_failure)?;
            if text.trim().is_empty() {
                return Err(definition_failure("没有读到定义"));
            }
            Ok(text)
        }
        "view" => read_postgres_text(
            client,
            "SELECT format(CASE WHEN c.relkind = 'm' THEN E'CREATE MATERIALIZED VIEW %I.%I AS\n%s;' ELSE E'CREATE OR REPLACE VIEW %I.%I AS\n%s;' END, n.nspname, c.relname, rtrim(btrim(pg_get_viewdef(c.oid, true)), ';')) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('v', 'm')",
            schema,
            name,
        )
        .await,
        _ => Err(definition_failure("只有视图、函数、存储过程、触发器、序列、枚举和域可以读取只读定义")),
    }
}

async fn read_postgres_text(client: &tokio_postgres::Client, sql: &str, schema: &str, name: &str) -> Result<String, String> {
    let rows = client.query(sql, &[&schema, &name]).await.map_err(definition_failure)?;
    let row = rows.first().ok_or_else(|| definition_failure("没有读到定义"))?;
    let text = pg_string(row, 0).map_err(definition_failure)?;
    if text.trim().is_empty() {
        Err(definition_failure("没有读到定义"))
    } else {
        Ok(text)
    }
}

fn postgres_object(row: &tokio_postgres::Row) -> Result<ObjectRow, String> {
    let name = pg_string(row, 0)?;
    let kind = pg_string(row, 1)?;
    let estimated = pg_i64_opt(row, 2)?;
    let signature = pg_text(row, 3);
    let materialized = signature == "materialized";
    Ok(ObjectRow {
        name,
        kind: parse_kind(&kind),
        estimated_rows: known_row_estimate(estimated),
        stale: false,
        signature: if materialized { String::new() } else { signature },
        relation: pg_text(row, 4),
        materialized,
    })
}

fn pg_string(row: &tokio_postgres::Row, index: usize) -> Result<String, String> {
    if let Ok(value) = row.try_get::<_, String>(index) {
        return Ok(value);
    }
    match row.try_get::<_, Option<String>>(index) {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Err(read_failure("元数据列是空值")),
        Err(err) => Err(read_failure(err)),
    }
}

fn pg_text(row: &tokio_postgres::Row, index: usize) -> String {
    row.try_get::<_, String>(index)
        .or_else(|_| row.try_get::<_, Option<String>>(index).map(|value| value.unwrap_or_default()))
        .unwrap_or_default()
}

fn pg_i64(row: &tokio_postgres::Row, index: usize) -> Result<i64, String> {
    row.try_get(index).map_err(read_failure)
}

fn pg_i64_opt(row: &tokio_postgres::Row, index: usize) -> Result<Option<i64>, String> {
    if let Ok(value) = row.try_get::<_, Option<i64>>(index) {
        return Ok(value);
    }
    row.try_get::<_, i64>(index).map(Some).map_err(read_failure)
}

/// 在目录会话上执行一条只读语句，把每格转成文本（对象详情与补全外键读取共用）。
pub(in crate::adapters) async fn text_rows(client: &tokio_postgres::Client, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
    use crate::catalog::detail::failure;
    let rows = client.query(sql, &[]).await.map_err(failure)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let width = row.len();
            (0..width)
                .map(|index| match row.try_get::<_, Option<String>>(index) {
                    Ok(value) => value,
                    Err(_) => row.try_get::<_, Option<i64>>(index).ok().flatten().map(|value| value.to_string()),
                })
                .collect()
        })
        .collect())
}
