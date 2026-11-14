//! 表/视图的按需列与索引读取。只在已经打开的 catalog 会话上发查询，
//! 不新开连接；失败只影响这一个对象，不碰同级的其它节点。

use mysql_async::prelude::Queryable;
use sqlx::Row as _;

use crate::catalog::hierarchy::{ColumnInfo, IndexInfo};
use crate::catalog::model::Engine;
use crate::connections::CatalogSession;
use crate::query::assist::{quote_ident, ColumnMeta};

/// SQL 字面量：单引号翻倍。目录读取只接受已读到的名字，这里仍然转义，
/// 避免对象名里的引号跑出语句。
fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn qualified(engine: Engine, database: &str, schema: &str, table: &str) -> String {
    let name = quote_ident(engine.as_str(), table);
    match engine {
        Engine::Sqlite => name,
        Engine::Mysql | Engine::Mariadb => {
            if database.is_empty() {
                name
            } else {
                format!("{}.{}", quote_ident("mysql", database), name)
            }
        }
        _ => {
            if schema.is_empty() {
                name
            } else {
                format!("{}.{}", quote_ident(engine.as_str(), schema), name)
            }
        }
    }
}

/// 列查询。SQLite 走 PRAGMA，其余引擎走 information_schema 的位置列：
/// name, type, nullable, default, comment。注释用左连接读，读不到就是 NULL，不影响列本身。
pub fn columns_sql(engine: Engine, database: &str, schema: &str, table: &str) -> Option<String> {
    match engine {
        Engine::Redis | Engine::Mongodb => None,
        Engine::Sqlite => Some(format!("PRAGMA table_info({})", quote_ident("sqlite", table))),
        Engine::Postgres => Some(format!(
            "SELECT c.column_name, c.data_type, c.is_nullable, c.column_default, d.description FROM information_schema.columns c LEFT JOIN pg_catalog.pg_namespace n ON n.nspname = c.table_schema LEFT JOIN pg_catalog.pg_class t ON t.relnamespace = n.oid AND t.relname = c.table_name LEFT JOIN pg_catalog.pg_description d ON d.objoid = t.oid AND d.classoid = 'pg_catalog.pg_class'::regclass AND d.objsubid = c.ordinal_position::int WHERE c.table_schema = {} AND c.table_name = {} ORDER BY c.ordinal_position",
            literal(schema),
            literal(table)
        )),
        Engine::Sqlserver => Some(format!(
            "SELECT c.column_name, c.data_type, c.is_nullable, c.column_default, CAST(ep.value AS nvarchar(4000)) FROM information_schema.columns c LEFT JOIN sys.extended_properties ep ON ep.class = 1 AND ep.name = 'MS_Description' AND ep.major_id = OBJECT_ID(QUOTENAME(c.table_schema) + '.' + QUOTENAME(c.table_name)) AND ep.minor_id = COLUMNPROPERTY(OBJECT_ID(QUOTENAME(c.table_schema) + '.' + QUOTENAME(c.table_name)), c.column_name, 'ColumnId') WHERE c.table_schema = {} AND c.table_name = {} ORDER BY c.ordinal_position",
            literal(schema),
            literal(table)
        )),
        Engine::Mysql | Engine::Mariadb => Some(format!(
            "SELECT column_name, data_type, is_nullable, column_default, column_comment FROM information_schema.columns WHERE table_schema = {} AND table_name = {} ORDER BY ordinal_position",
            literal(database),
            literal(table)
        )),
    }
}

/// 一个库/模式里表和视图的注释：(name, comment)，只列有注释的对象。SQLite 和非关系引擎没有注释。
/// MySQL 视图的 table_comment 固定是 VIEW，不是注释，排除掉。
pub fn table_comments_sql(engine: Engine, database: &str, schema: &str) -> Option<String> {
    match engine {
        Engine::Redis | Engine::Mongodb | Engine::Sqlite => None,
        Engine::Postgres => Some(format!(
            "SELECT t.relname, d.description FROM pg_catalog.pg_class t JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace JOIN pg_catalog.pg_description d ON d.objoid = t.oid AND d.classoid = 'pg_catalog.pg_class'::regclass AND d.objsubid = 0 WHERE n.nspname = {} AND t.relkind IN ('r', 'p', 'v', 'm', 'f') ORDER BY t.relname",
            literal(schema)
        )),
        Engine::Sqlserver => Some(format!(
            "SELECT o.name, CAST(ep.value AS nvarchar(4000)) FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id JOIN sys.extended_properties ep ON ep.class = 1 AND ep.major_id = o.object_id AND ep.minor_id = 0 AND ep.name = 'MS_Description' WHERE s.name = {} AND o.type IN ('U', 'V') ORDER BY o.name",
            literal(schema)
        )),
        Engine::Mysql | Engine::Mariadb => Some(format!(
            "SELECT table_name, table_comment FROM information_schema.tables WHERE table_schema = {} AND table_type <> 'VIEW' AND table_comment <> '' ORDER BY table_name",
            literal(database)
        )),
    }
}

/// SQL 补全只读取当前涉及对象的注释，不预读整个库。
pub fn table_comment_sql(engine: Engine, database: &str, schema: &str, table: &str) -> Option<String> {
    let sql = table_comments_sql(engine, database, schema)?;
    let (select, order) = sql.rsplit_once(" ORDER BY ")?;
    let column = match engine { Engine::Postgres => "t.relname", Engine::Sqlserver => "o.name", _ => "table_name" };
    Some(format!("{select} AND {column} = {} ORDER BY {order}", literal(table)))
}

/// 主键列查询。没有主键就是空集，不是失败。
pub fn primary_key_sql(engine: Engine, database: &str, schema: &str, table: &str) -> Option<String> {
    let scope = match engine {
        Engine::Postgres | Engine::Sqlserver => literal(schema),
        Engine::Mysql | Engine::Mariadb => literal(database),
        _ => return None,
    };
    Some(format!(
        "SELECT kcu.column_name FROM information_schema.table_constraints tc JOIN information_schema.key_column_usage kcu ON kcu.constraint_name = tc.constraint_name AND kcu.table_schema = tc.table_schema AND kcu.table_name = tc.table_name WHERE tc.constraint_type = 'PRIMARY KEY' AND tc.table_schema = {scope} AND tc.table_name = {}",
        literal(table)
    ))
}

/// 索引查询。SQLite 先列再逐个读列；MySQL 用 SHOW INDEX。
pub fn indexes_sql(engine: Engine, database: &str, schema: &str, table: &str) -> Option<String> {
    match engine {
        Engine::Redis | Engine::Mongodb => None,
        Engine::Sqlite => Some(format!("PRAGMA index_list({})", quote_ident("sqlite", table))),
        Engine::Mysql | Engine::Mariadb => Some(format!("SHOW INDEX FROM {}", qualified(engine, database, schema, table))),
        Engine::Postgres => Some(format!(
            "SELECT i.relname, a.attname, ix.indisunique::text, ix.indisprimary::text, k.ord::text FROM pg_index ix JOIN pg_class i ON i.oid = ix.indexrelid JOIN pg_class t ON t.oid = ix.indrelid JOIN pg_namespace n ON n.oid = t.relnamespace LEFT JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY AS k(attnum, ord) ON true LEFT JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum WHERE n.nspname = {} AND t.relname = {} ORDER BY i.relname, k.ord",
            literal(schema),
            literal(table)
        )),
        Engine::Sqlserver => Some(format!(
            "SELECT i.name, c.name, i.is_unique, i.is_primary_key FROM sys.indexes i JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id JOIN sys.columns c ON c.object_id = i.object_id AND c.column_id = ic.column_id JOIN sys.tables t ON t.object_id = i.object_id JOIN sys.schemas s ON s.schema_id = t.schema_id WHERE s.name = {} AND t.name = {} AND i.name IS NOT NULL ORDER BY i.name, ic.key_ordinal",
            literal(schema),
            literal(table)
        )),
    }
}

pub fn sqlite_index_info_sql(index: &str) -> String {
    format!("PRAGMA index_info({})", quote_ident("sqlite", index))
}

fn cell(rows: &[Vec<Option<String>>], row: usize, column: usize) -> String {
    rows.get(row)
        .and_then(|row| row.get(column))
        .and_then(|cell| cell.clone())
        .unwrap_or_default()
}

fn yes(text: &str) -> bool {
    matches!(text.trim().to_ascii_uppercase().as_str(), "YES" | "TRUE" | "1" | "T")
}

/// information_schema 的位置列：name, type, nullable, default；
/// SQLite 的 PRAGMA table_info 是 cid, name, type, notnull, default, pk。
pub fn columns_from_rows(engine: Engine, rows: &[Vec<Option<String>>]) -> Vec<ColumnInfo> {
    let mut columns = Vec::new();
    for index in 0..rows.len() {
        let (name, data_type, nullable, primary) = match engine {
            Engine::Sqlite => {
                let notnull = cell(rows, index, 3).trim() == "1";
                let pk = {
                    let value = cell(rows, index, 5);
                    !value.is_empty() && value.trim() != "0"
                };
                (cell(rows, index, 1), cell(rows, index, 2), !notnull, pk)
            }
            _ => (cell(rows, index, 0), cell(rows, index, 1), yes(&cell(rows, index, 2)), false),
        };
        if name.is_empty() {
            continue;
        }
        columns.push(ColumnInfo {
            name,
            data_type,
            nullable,
            primary,
        });
    }
    columns
}

/// 补全和悬停用的列元数据：在 columns_from_rows 的位置约定上再取注释（第 5 列）。
/// SQLite 没有列注释，主键在 PRAGMA 的 pk 列；其它引擎的主键由 apply_primary_to_meta 另查。
/// 读不到的类型和注释留空，不编造。
pub fn column_meta_from_rows(engine: Engine, rows: &[Vec<Option<String>>]) -> Vec<ColumnMeta> {
    let mut columns = Vec::new();
    for index in 0..rows.len() {
        let (name, type_name, nullable, comment, primary) = match engine {
            Engine::Sqlite => {
                let notnull = cell(rows, index, 3);
                let nullable = match notnull.trim() {
                    "" => None,
                    flag => Some(flag != "1"),
                };
                let pk = cell(rows, index, 5);
                let primary = !pk.trim().is_empty() && pk.trim() != "0";
                (cell(rows, index, 1), cell(rows, index, 2), nullable, String::new(), primary)
            }
            _ => {
                let flag = cell(rows, index, 2);
                let nullable = if flag.trim().is_empty() { None } else { Some(yes(&flag)) };
                (cell(rows, index, 0), cell(rows, index, 1), nullable, cell(rows, index, 4), false)
            }
        };
        if name.is_empty() {
            continue;
        }
        columns.push(ColumnMeta { name, type_name, nullable, comment, primary, foreign_key: false });
    }
    columns
}

/// 主键查询的列名行标到已读列上。对不上的名字不编造列。
pub fn apply_primary_to_meta(columns: &mut [ColumnMeta], rows: &[Vec<Option<String>>]) {
    for index in 0..rows.len() {
        let name = cell(rows, index, 0);
        if name.is_empty() {
            continue;
        }
        for column in columns.iter_mut() {
            if column.name.eq_ignore_ascii_case(&name) {
                column.primary = true;
            }
        }
    }
}

/// (name, comment) 行收成对照表；名字或注释为空的行跳过。
pub fn comments_from_rows(rows: &[Vec<Option<String>>]) -> Vec<(String, String)> {
    (0..rows.len())
        .map(|index| (cell(rows, index, 0), cell(rows, index, 1)))
        .filter(|(name, comment)| !name.is_empty() && !comment.trim().is_empty())
        .collect()
}

pub fn apply_primary_key(columns: &mut [ColumnInfo], rows: &[Vec<Option<String>>]) {
    for index in 0..rows.len() {
        let name = cell(rows, index, 0);
        for column in columns.iter_mut() {
            if column.name == name {
                column.primary = true;
            }
        }
    }
}

/// 索引行收成索引：兼容 SQLite/MySQL/PG/SQLServer 的列顺序。
/// (name, column, unique, primary, ordinal)
pub fn indexes_from_rows(engine: Engine, rows: &[Vec<Option<String>>]) -> Vec<IndexInfo> {
    let mut indexes: Vec<IndexInfo> = Vec::new();
    let mut positions: Vec<Vec<(i64, String)>> = Vec::new();
    for index in 0..rows.len() {
        let (name, column, unique, primary, ordinal) = match engine {
            Engine::Sqlite => (cell(rows, index, 1), String::new(), yes(&cell(rows, index, 2)), false, 0),
            Engine::Mysql | Engine::Mariadb => (
                cell(rows, index, 2),
                cell(rows, index, 4),
                cell(rows, index, 1).trim() == "0",
                cell(rows, index, 2).eq_ignore_ascii_case("PRIMARY"),
                cell(rows, index, 3).parse::<i64>().unwrap_or(0),
            ),
            // PostgreSQL / SQL Server: name, column, unique, primary, ordinal
            _ => (
                cell(rows, index, 0),
                cell(rows, index, 1),
                yes(&cell(rows, index, 2)),
                yes(&cell(rows, index, 3)),
                cell(rows, index, 4).parse::<i64>().unwrap_or(0),
            ),
        };
        if name.is_empty() {
            continue;
        }
        match indexes.iter().position(|item| item.name == name) {
            Some(at) => {
                if !column.is_empty() {
                    positions[at].push((ordinal, column));
                }
                indexes[at].unique |= unique;
                indexes[at].primary |= primary;
            }
            None => {
                indexes.push(IndexInfo {
                    name,
                    columns: Vec::new(),
                    unique,
                    primary,
                });
                positions.push(if column.is_empty() { Vec::new() } else { vec![(ordinal, column)] });
            }
        }
    }
    for (at, mut items) in positions.into_iter().enumerate() {
        items.sort_by_key(|(ordinal, _)| *ordinal);
        indexes[at].columns = items.into_iter().map(|(_, column)| column).collect();
    }
    indexes
}

fn failure(err: impl std::fmt::Display) -> String {
    format!("读取对象详情失败：{err}")
}

fn mysql_cell(value: Option<&mysql_async::Value>) -> Option<String> {
    match value? {
        mysql_async::Value::NULL => None,
        mysql_async::Value::Bytes(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        mysql_async::Value::Int(number) => Some(number.to_string()),
        mysql_async::Value::UInt(number) => Some(number.to_string()),
        mysql_async::Value::Float(number) => Some(number.to_string()),
        mysql_async::Value::Double(number) => Some(number.to_string()),
        other => Some(format!("{other:?}")),
    }
}

fn mysql_row(row: &mysql_async::Row, index: usize) -> Option<String> {
    match row.as_ref(index) {
        Some(mysql_async::Value::NULL) | None => None,
        Some(mysql_async::Value::Int(number)) => Some(number.to_string()),
        Some(mysql_async::Value::UInt(number)) => Some(number.to_string()),
        Some(value) => mysql_cell(Some(value)),
    }
}

fn sqlite_row(row: &sqlx::sqlite::SqliteRow, index: usize) -> Option<String> {
    if let Ok(value) = row.try_get::<Option<String>, _>(index) {
        return value;
    }
    if let Ok(value) = row.try_get::<Option<i64>, _>(index) {
        return value.map(|number| number.to_string());
    }
    if let Ok(value) = row.try_get::<Option<f64>, _>(index) {
        return value.map(|number| number.to_string());
    }
    None
}

/// 在一个已经打开的目录会话上执行一条只读语句，把每格转成文本。
/// 同模块的按需读取和查询补全的外键读取共用它，不再各写一份引擎分支。
pub(crate) async fn rows_for(session: &CatalogSession, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
    match session {
        CatalogSession::Scripted => Err(failure("这个连接没有连着可查询的数据库")),
        CatalogSession::Redis(_) => Err(failure("Redis 没有列或索引")),
        CatalogSession::Mongo(_) => Err(failure("MongoDB 没有 SQL 列或索引")),
        CatalogSession::Sqlite { pool, .. } => {
            let rows = sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
                .fetch_all(pool)
                .await
                .map_err(failure)?;
            Ok(rows
                .into_iter()
                .map(|row| {
                    let width = row.columns().len();
                    (0..width).map(|index| sqlite_row(&row, index)).collect()
                })
                .collect())
        }
        CatalogSession::Mysql(pool) => {
            let mut conn = pool.get_conn().await.map_err(failure)?;
            let rows: Vec<mysql_async::Row> = conn.query(sql).await.map_err(failure)?;
            Ok(rows
                .into_iter()
                .map(|row| {
                    let width = row.columns_ref().len().max(row.len());
                    (0..width).map(|index| mysql_row(&row, index)).collect()
                })
                .collect())
        }
        CatalogSession::Postgres(client) => {
            let rows = client.query(sql, &[]).await.map_err(failure)?;
            Ok(rows
                .into_iter()
                .map(|row| {
                    let width = row.len();
                    (0..width)
                        .map(|index| match row.try_get::<_, Option<String>>(index) {
                            Ok(value) => value,
                            Err(_) => row
                                .try_get::<_, Option<i64>>(index)
                                .ok()
                                .flatten()
                                .map(|value| value.to_string()),
                        })
                        .collect()
                })
                .collect())
        }
        CatalogSession::Sqlserver(client) => {
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
    }
}

/// 读一个表的列元数据（名字、类型、可空、注释），供补全详情、悬停和对象信息使用。
pub async fn read_column_meta(
    session: &CatalogSession,
    engine: Engine,
    database: &str,
    schema: &str,
    table: &str,
) -> Result<Vec<ColumnMeta>, String> {
    // MongoDB / Redis 没有列清单：只是没有列元数据，不是读取失败（与注释同一处理）。
    // 当成失败会让对象树顶端常驻“读取对象失败”。
    let Some(sql) = columns_sql(engine, database, schema, table) else {
        return Ok(Vec::new());
    };
    let rows = rows_for(session, &sql).await?;
    let mut meta = column_meta_from_rows(engine, &rows);
    // 主键读不到不算列失败：列已经读到了，只是没有主键标记。SQLite 的 pk 已在 PRAGMA 行里。
    if let Some(pk_sql) = primary_key_sql(engine, database, schema, table) {
        if let Ok(pk_rows) = rows_for(session, &pk_sql).await {
            apply_primary_to_meta(&mut meta, &pk_rows);
        }
    }
    Ok(meta)
}

/// 读一个库/模式里对象的注释。没有注释来源的引擎返回空表，不是失败。
pub async fn read_table_comments(session: &CatalogSession, engine: Engine, database: &str, schema: &str) -> Result<Vec<(String, String)>, String> {
    let Some(sql) = table_comments_sql(engine, database, schema) else {
        return Ok(Vec::new());
    };
    let rows = rows_for(session, &sql).await?;
    Ok(comments_from_rows(&rows))
}

pub async fn read_table_comment(session: &CatalogSession, engine: Engine, database: &str, schema: &str, table: &str) -> Result<Option<String>, String> {
    let Some(sql) = table_comment_sql(engine, database, schema, table) else { return Ok(None) };
    let rows = rows_for(session, &sql).await?;
    Ok(comments_from_rows(&rows).into_iter().next().map(|(_, comment)| comment))
}

/// SQL Server 的列/索引/主键查询里既有目录视图也有只在当前库生效的函数：
/// 读非当前库时按目标库三段名限定，同一条会话、不发 USE。
fn scoped_sql(engine: Engine, database: &str, sql: &str) -> String {
    if engine == Engine::Sqlserver {
        crate::catalog::read::sqlserver_qualified(sql, database)
    } else {
        sql.to_owned()
    }
}

/// 读一个对象的列与索引。两边分别成败：列失败不影响索引结果；两者都不碰同级的其它节点。
/// database 是节点自己的库：SQL Server 用它限定三段名；PostgreSQL 非当前库要用连到该库的会话。
pub async fn read_object_details(
    session: &CatalogSession,
    engine: Engine,
    database: &str,
    schema: &str,
    table: &str,
) -> (Result<Vec<ColumnInfo>, String>, Result<Vec<IndexInfo>, String>) {
    let columns = match columns_sql(engine, database, schema, table) {
        None => Err("这个引擎没有列清单".to_owned()),
        Some(sql) => match rows_for(session, &scoped_sql(engine, database, &sql)).await {
            Ok(rows) => {
                let mut items = columns_from_rows(engine, &rows);
                if let Some(pk_sql) = primary_key_sql(engine, database, schema, table) {
                    let pk_sql = scoped_sql(engine, database, &pk_sql);
                    // 主键读不到不算列失败：列已经读到了，只是没有主键标记。
                    if let Ok(pk_rows) = rows_for(session, &pk_sql).await {
                        apply_primary_key(&mut items, &pk_rows);
                    }
                }
                Ok(items)
            }
            Err(message) => Err(message),
        },
    };
    let indexes = match indexes_sql(engine, database, schema, table) {
        None => Err("这个引擎没有索引清单".to_owned()),
        Some(sql) => match rows_for(session, &scoped_sql(engine, database, &sql)).await {
            Ok(rows) => {
                let mut items = indexes_from_rows(engine, &rows);
                if engine == Engine::Sqlite {
                    for item in items.iter_mut() {
                        if let Ok(rows) = rows_for(session, &sqlite_index_info_sql(&item.name)).await {
                            item.columns = (0..rows.len())
                                .map(|index| cell(&rows, index, 2))
                                .filter(|name| !name.is_empty())
                                .collect();
                        }
                    }
                }
                Ok(items)
            }
            Err(message) => Err(message),
        },
    };
    (columns, indexes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 没有列清单的引擎（MongoDB / Redis）读列元数据得到空表而不是失败：
    /// 否则连上 MongoDB 后对象树顶端会常驻“读取对象失败”。
    #[tokio::test]
    async fn column_meta_of_non_relational_engines_is_empty_not_a_failure() {
        for engine in [Engine::Mongodb, Engine::Redis] {
            let meta = read_column_meta(&CatalogSession::Scripted, engine, "app", "", "users").await;
            assert_eq!(meta, Ok(Vec::new()), "{engine:?}");
        }
    }

    #[test]
    fn table_comment_is_scoped_and_quotes_untrusted_names() {
        for engine in [Engine::Mysql, Engine::Mariadb, Engine::Postgres, Engine::Sqlserver] {
            let sql = table_comment_sql(engine, "shop", "public", "a'b").unwrap();
            assert!(sql.contains(" = 'a''b' ORDER BY "), "{sql}");
        }
        assert!(table_comment_sql(Engine::Sqlite, "main", "", "item").is_none());
    }

    #[test]
    fn engine_sql_shapes_quote_names_and_skip_non_relational_engines() {
        assert!(columns_sql(Engine::Sqlite, "main", "", "notes").unwrap().contains("PRAGMA table_info"));
        let pg = columns_sql(Engine::Postgres, "shop", "public", "orders").unwrap();
        assert!(pg.contains("table_schema = 'public'"));
        assert!(pg.contains("table_name = 'orders'"));
        let my = columns_sql(Engine::Mysql, "shop", "", "orders").unwrap();
        assert!(my.contains("table_schema = 'shop'"));
        assert!(indexes_sql(Engine::Redis, "0", "", "k").is_none());
        assert!(indexes_sql(Engine::Mongodb, "app", "", "users").is_none());

        // 名字里的引号不能跑出字面量。
        let quoted = columns_sql(Engine::Postgres, "shop", "pub'lic", "o'rders").unwrap();
        assert!(quoted.contains("'pub''lic'"), "{quoted}");
        assert!(quoted.contains("'o''rders'"), "{quoted}");
        let sqlite = columns_sql(Engine::Sqlite, "main", "", "we\"ird").unwrap();
        assert_eq!(sqlite, "PRAGMA table_info(\"we\"\"ird\")");
    }

    #[test]
    fn information_schema_rows_become_columns_and_primary_keys_mark_them() {
        let rows = vec![
            vec![Some("id".to_owned()), Some("integer".to_owned()), Some("NO".to_owned()), None],
            vec![Some("name".to_owned()), Some("text".to_owned()), Some("YES".to_owned()), None],
        ];
        let mut columns = columns_from_rows(Engine::Postgres, &rows);
        assert_eq!(columns.len(), 2);
        assert_eq!(columns[0].data_type, "integer");
        assert!(!columns[0].nullable);
        assert!(columns[1].nullable);
        assert!(columns.iter().all(|column| !column.primary));
        apply_primary_key(&mut columns, &[vec![Some("id".to_owned())]]);
        assert!(columns[0].primary);
        assert!(!columns[1].primary);
    }

    #[test]
    fn column_meta_reads_type_nullability_and_comment_without_inventing_any() {
        let rows = vec![
            vec![Some("id".to_owned()), Some("integer".to_owned()), Some("NO".to_owned()), None, Some("主键".to_owned())],
            vec![Some("note".to_owned()), Some("text".to_owned()), Some("YES".to_owned()), None, None],
            vec![Some("legacy".to_owned()), None, None, None],
        ];
        let mut meta = column_meta_from_rows(Engine::Postgres, &rows);
        assert_eq!(meta.len(), 3);
        assert_eq!(
            meta[0],
            ColumnMeta { name: "id".to_owned(), type_name: "integer".to_owned(), nullable: Some(false), comment: "主键".to_owned(), primary: false, foreign_key: false }
        );
        assert_eq!(meta[1].nullable, Some(true));
        assert!(meta[1].comment.is_empty(), "没有注释就留空");
        assert!(!meta[0].primary, "information_schema 行本身不带主键");
        assert_eq!(meta[2], ColumnMeta::named("legacy"), "读不到的类型和可空不编造");
        apply_primary_to_meta(&mut meta, &[vec![Some("id".to_owned())]]);
        assert!(meta[0].primary);
        assert!(!meta[1].primary);
        assert!(!meta[0].foreign_key, "外键不从列查询编造");

        let sqlite = vec![
            vec![
                Some("0".to_owned()),
                Some("id".to_owned()),
                Some("INTEGER".to_owned()),
                Some("1".to_owned()),
                None,
                Some("1".to_owned()),
            ],
            vec![
                Some("1".to_owned()),
                Some("body".to_owned()),
                Some("TEXT".to_owned()),
                Some("0".to_owned()),
                None,
                Some("0".to_owned()),
            ],
        ];
        let meta = column_meta_from_rows(Engine::Sqlite, &sqlite);
        assert!(meta[0].primary, "PRAGMA pk 非 0 是主键");
        assert_eq!(meta[0].nullable, Some(false));
        assert!(meta[0].comment.is_empty(), "SQLite 没有列注释");
        assert!(!meta[1].primary);
        assert_eq!(meta[1].nullable, Some(true));
    }

    #[test]
    fn column_and_table_comments_come_from_each_engine_catalog() {
        let pg = columns_sql(Engine::Postgres, "shop", "public", "orders").unwrap();
        assert!(pg.contains("pg_description"), "{pg}");
        assert!(pg.contains("LEFT JOIN"), "注释读不到也要有列：{pg}");
        let my = columns_sql(Engine::Mysql, "shop", "", "orders").unwrap();
        assert!(my.contains("column_comment"), "{my}");
        let ms = columns_sql(Engine::Sqlserver, "shop", "dbo", "orders").unwrap();
        assert!(ms.contains("MS_Description") && ms.contains("LEFT JOIN"), "{ms}");
        assert!(table_comments_sql(Engine::Sqlite, "main", "").is_none());
        assert!(table_comments_sql(Engine::Redis, "0", "").is_none());
        let tables = table_comments_sql(Engine::Mysql, "sh'op", "").unwrap();
        assert!(tables.contains("'sh''op'") && tables.contains("table_type <> 'VIEW'"), "{tables}");
        let rows = vec![
            vec![Some("orders".to_owned()), Some("订单".to_owned())],
            vec![Some("blank".to_owned()), Some(" ".to_owned())],
            vec![None, Some("x".to_owned())],
        ];
        assert_eq!(comments_from_rows(&rows), vec![("orders".to_owned(), "订单".to_owned())]);
    }

    #[test]
    fn sqlite_columns_read_notnull_and_pk_flags() {
        // PRAGMA table_info: cid, name, type, notnull, dflt, pk
        let rows = vec![
            vec![
                Some("0".to_owned()),
                Some("id".to_owned()),
                Some("INTEGER".to_owned()),
                Some("0".to_owned()),
                None,
                Some("1".to_owned()),
            ],
            vec![
                Some("1".to_owned()),
                Some("body".to_owned()),
                Some("TEXT".to_owned()),
                Some("1".to_owned()),
                None,
                Some("0".to_owned()),
            ],
        ];
        let columns = columns_from_rows(Engine::Sqlite, &rows);
        assert_eq!(columns[0].name, "id");
        assert!(columns[0].primary);
        assert!(columns[0].nullable, "notnull=0 表示可以为 NULL");
        assert!(!columns[1].primary);
        assert!(!columns[1].nullable, "notnull=1 表示不允许 NULL");
    }

    #[test]
    fn index_rows_group_columns_in_ordinal_order() {
        let rows = vec![
            vec![Some("ix_a".to_owned()), Some("b".to_owned()), Some("t".to_owned()), Some("f".to_owned()), Some("2".to_owned())],
            vec![Some("ix_a".to_owned()), Some("a".to_owned()), Some("t".to_owned()), Some("f".to_owned()), Some("1".to_owned())],
            vec![Some("ix_pk".to_owned()), Some("id".to_owned()), Some("t".to_owned()), Some("t".to_owned()), Some("1".to_owned())],
        ];
        let indexes = indexes_from_rows(Engine::Postgres, &rows);
        assert_eq!(indexes.len(), 2);
        assert_eq!(indexes[0].name, "ix_a");
        assert!(indexes[0].unique);
        assert_eq!(indexes[0].columns, vec!["a", "b"]);
        assert!(indexes[1].primary);
    }

    #[test]
    fn mysql_index_rows_use_show_index_positions() {
        // SHOW INDEX: table, non_unique, key_name, seq, column_name ...
        let rows = vec![
            vec![Some("orders".to_owned()), Some("0".to_owned()), Some("PRIMARY".to_owned()), Some("1".to_owned()), Some("id".to_owned())],
            vec![Some("orders".to_owned()), Some("1".to_owned()), Some("ix_name".to_owned()), Some("1".to_owned()), Some("name".to_owned())],
        ];
        let indexes = indexes_from_rows(Engine::Mysql, &rows);
        assert_eq!(indexes.len(), 2);
        assert!(indexes[0].primary && indexes[0].unique);
        assert_eq!(indexes[0].columns, vec!["id"]);
        assert!(!indexes[1].unique);
        assert_eq!(indexes[1].columns, vec!["name"]);
    }
}