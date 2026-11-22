//! SQLite 的表网格读写：描述表结构、读一页、计数、逐条执行、外键与流式导出。都在已打开的连接池上进行。

use std::collections::HashMap;

use futures::TryStreamExt;
use sqlx::Column as _;
use sqlx::Executor;
use sqlx::Row as _;
use sqlx::SqlSafeStr;
use sqlx::Statement as _;

use crate::adapters::grid::{audited, cell_text, db_err, other_database, ExportSink};
use crate::adapters::SqlSyntax;
use crate::connections::model::Engine;
use crate::grid::{push_foreign_key, Cell, Column, ForeignKey, TableSchema};

use super::syntax::SqliteSyntax;

fn quote(name: &str) -> String {
    SqliteSyntax.quote_ident(name)
}

pub(in crate::adapters) async fn describe(pool: &sqlx::SqlitePool, database: &str, table: &str) -> Result<TableSchema, String> {
    if !database.is_empty() && database != "main" {
        return Err(other_database("main", database));
    }
    describe_sqlite(pool, table).await
}

pub(in crate::adapters) async fn fetch_cells(pool: &sqlx::SqlitePool, sql: &str, width: usize) -> Result<Vec<Vec<Cell>>, String> {
    let mut stream = sqlx::query(audited(sql)).fetch(pool);
    let mut out = Vec::new();
    while let Some(row) = stream.try_next().await.map_err(db_err)? {
        out.push(read_width(&row, width, sqlite_cell)?);
    }
    Ok(out)
}

pub(in crate::adapters) async fn fetch_count(pool: &sqlx::SqlitePool, sql: &str) -> Result<i64, String> {
    sqlx::query_scalar(audited(sql)).fetch_one(pool).await.map_err(db_err)
}

pub(in crate::adapters) async fn execute(pool: &sqlx::SqlitePool, sql: &str) -> Result<u64, String> {
    sqlx::query(audited(sql)).execute(pool).await.map(|done| done.rows_affected()).map_err(db_err)
}

/// 表级出向外键：PRAGMA foreign_key_list；引用父表主键时 to 为 NULL，按 seq 取父表主键列。
pub(in crate::adapters) async fn foreign_keys(pool: &sqlx::SqlitePool, database: &str, table: &str) -> Result<Vec<ForeignKey>, String> {
    if !database.is_empty() && database != "main" {
        return Err(other_database("main", database));
    }
    let quoted = quote(table);
    let rows = sqlx::query(audited(&format!("PRAGMA foreign_key_list({quoted})")))
        .fetch_all(pool)
        .await
        .map_err(db_err)?;
    let mut list: Vec<ForeignKey> = Vec::new();
    let mut pk_cache: HashMap<String, Vec<String>> = HashMap::new();
    for row in rows {
        let id: i64 = row.try_get("id").map_err(db_err)?;
        let seq: i64 = row.try_get("seq").unwrap_or(0);
        let column: String = row.try_get("from").map_err(db_err)?;
        let ref_table: String = row.try_get("table").map_err(db_err)?;
        let explicit: Option<String> = row.try_get("to").ok().flatten();
        let ref_column = match explicit {
            Some(name) if !name.is_empty() => name,
            // 引用父表主键时 PRAGMA 的 to 为 NULL：按 seq 取父表主键序号，找不到就跳过。
            _ => {
                if !pk_cache.contains_key(&ref_table) {
                    let parent = quote(&ref_table);
                    let info = sqlx::query(audited(&format!("PRAGMA table_info({parent})")))
                        .fetch_all(pool)
                        .await
                        .map_err(db_err)?;
                    let mut keys: Vec<(i64, String)> = info
                        .iter()
                        .filter_map(|item| {
                            let pk = item.try_get::<i64, _>("pk").unwrap_or(0);
                            if pk > 0 { item.try_get::<String, _>("name").ok().map(|name| (pk, name)) } else { None }
                        })
                        .collect();
                    keys.sort_by_key(|(pk, _)| *pk);
                    pk_cache.insert(ref_table.clone(), keys.into_iter().map(|(_, name)| name).collect());
                }
                match pk_cache.get(&ref_table).and_then(|keys| keys.get(seq as usize)) {
                    Some(name) => name.clone(),
                    None => continue,
                }
            }
        };
        push_foreign_key(&mut list, &format!("fk_{id}"), &column, "main", &ref_table, &ref_column);
    }
    Ok(list)
}

/// 流式导出：列名已知时直接用，否则先 prepare 取列名；逐行读、逐行写。
pub(in crate::adapters) async fn stream(pool: &sqlx::SqlitePool, sql: &str, known_names: Option<Vec<String>>, sink: &mut dyn ExportSink) {
    let names = match known_names {
        Some(names) => names,
        None => {
            let mut conn = match pool.acquire().await {
                Ok(conn) => conn,
                Err(err) => return sink.fail(&crate::error_text::chain(&err)),
            };
            let statement = match conn.prepare(audited(sql).into_sql_str()).await {
                Ok(statement) => statement,
                Err(err) => return sink.fail(&crate::error_text::chain(&err)),
            };
            statement.columns().iter().map(|column| column.name().to_owned()).collect()
        }
    };
    if !sink.start(&names) {
        return;
    }
    let mut stream = sqlx::query(audited(sql)).fetch(pool);
    loop {
        if !sink.proceed() {
            return;
        }
        match stream.try_next().await {
            Ok(Some(row)) => {
                let cells = match read_width(&row, names.len(), sqlite_cell) {
                    Ok(cells) => cells,
                    Err(message) => return sink.fail(&message),
                };
                if !sink.row(&cells) {
                    return;
                }
            }
            Ok(None) => return sink.finish(),
            Err(err) => return sink.fail(&crate::error_text::chain(&err)),
        }
    }
}

fn read_width(row: &sqlx::sqlite::SqliteRow, width: usize, read: fn(&sqlx::sqlite::SqliteRow, usize) -> Result<Cell, String>) -> Result<Vec<Cell>, String> {
    (0..width).map(|index| read(row, index)).collect()
}

fn sqlite_cell(row: &sqlx::sqlite::SqliteRow, index: usize) -> Result<Cell, String> {
    use sqlx::Row;
    if let Ok(value) = row.try_get::<Option<String>, _>(index) {
        return Ok(cell_text(value));
    }
    if let Ok(value) = row.try_get::<Option<i64>, _>(index) {
        return Ok(cell_text(value.map(|number| number.to_string())));
    }
    if let Ok(value) = row.try_get::<Option<f64>, _>(index) {
        return Ok(cell_text(value.map(|number| number.to_string())));
    }
    if let Ok(value) = row.try_get::<Option<Vec<u8>>, _>(index) {
        return Ok(cell_text(value.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())));
    }
    Err(format!("读不到第 {index} 列"))
}

async fn describe_sqlite(pool: &sqlx::SqlitePool, table: &str) -> Result<TableSchema, String> {
    let kind: Option<String> = sqlx::query_scalar("SELECT type FROM sqlite_master WHERE name = ?1 AND type IN ('table', 'view')")
        .bind(table)
        .fetch_optional(pool)
        .await
        .map_err(db_err)?;
    let kind = kind.ok_or_else(|| format!("找不到表或视图 {table}"))?;
    let kind = if kind.eq_ignore_ascii_case("view") { "view" } else { "table" };
    let quoted = quote(table);
    // table_xinfo 比 table_info 多一列 hidden：2/3 是生成列（虚拟/存储），1 是虚拟表的隐藏列（不显示）。
    let info = sqlx::query(audited(&format!("PRAGMA table_xinfo({quoted})"))).fetch_all(pool).await.map_err(db_err)?;
    if info.is_empty() {
        return Err(format!("找不到表或视图 {table}"));
    }
    let mut pk_order: Vec<(i64, String)> = Vec::new();
    let mut columns = Vec::new();
    for row in &info {
        let name = match sqlite_cell(row, 1)? {
            Cell::Text(text) => text,
            Cell::Null => return Err("列名是空的".to_owned()),
        };
        let type_name = match sqlite_cell(row, 2)? {
            Cell::Text(text) => text,
            Cell::Null => String::new(),
        };
        let not_null = sqlite_number(row, 3)? != 0;
        let raw_default = match sqlite_cell(row, 4)? {
            Cell::Text(text) => Some(text),
            Cell::Null => None,
        };
        let pk = sqlite_number(row, 5)?;
        let hidden = sqlite_number(row, 6).unwrap_or(0);
        if hidden == 1 {
            continue;
        }
        if pk > 0 {
            pk_order.push((pk, name.clone()));
        }
        let (generated, default) = column_traits(hidden, raw_default.as_deref());
        columns.push(Column {
            name,
            type_name,
            nullable: !not_null,
            primary: pk > 0,
            unique: false,
            auto: false,
            default,
            generated,
            choices: Vec::new(),
        });
    }
    pk_order.sort_by_key(|(order, _)| *order);
    let mut key: Vec<String> = pk_order.into_iter().map(|(_, name)| name).collect();
    let indexes = sqlx::query(audited(&format!("PRAGMA index_list({quoted})"))).fetch_all(pool).await.map_err(db_err)?;
    let mut unique_columns: Vec<Vec<String>> = Vec::new();
    for index in &indexes {
        let unique = sqlite_number(index, 2)? != 0;
        let partial = sqlite_number(index, 4).unwrap_or(0) != 0;
        if !unique || partial {
            continue;
        }
        let index_name = match sqlite_cell(index, 1)? {
            Cell::Text(text) => text,
            Cell::Null => continue,
        };
        let origin = match sqlite_cell(index, 3) {
            Ok(Cell::Text(text)) => text,
            _ => String::new(),
        };
        let index_rows = sqlx::query(audited(&format!("PRAGMA index_info({})", quote(&index_name))))
            .fetch_all(pool)
            .await
            .map_err(db_err)?;
        let mut pairs = Vec::new();
        for index_row in &index_rows {
            let seq = sqlite_number(index_row, 0)?;
            let name = match sqlite_cell(index_row, 2)? {
                Cell::Text(text) => text,
                Cell::Null => continue,
            };
            pairs.push((seq, name));
        }
        pairs.sort_by_key(|(seq, _)| *seq);
        let names = pairs.into_iter().map(|(_, name)| name).collect::<Vec<_>>();
        if origin == "pk" && key.is_empty() {
            key = names.clone();
        }
        unique_columns.push(names);
    }
    if key.is_empty() {
        if let Some(first) = unique_columns.first() {
            key = first.clone();
        }
    }
    let single_unique = unique_columns.iter().filter(|group| group.len() == 1).map(|group| group[0].clone()).collect::<Vec<_>>();
    let key_for_flags = key.clone();
    for column in &mut columns {
        column.unique = column.primary || single_unique.iter().any(|name| name == &column.name);
        column.auto = column.primary && key_for_flags.len() == 1 && column.type_name.to_ascii_lowercase().contains("int");
    }
    Ok(TableSchema { engine: Engine::Sqlite, schema: String::new(), name: table.to_owned(), kind, columns, key })
}

fn sqlite_number(row: &sqlx::sqlite::SqliteRow, index: usize) -> Result<i64, String> {
    match sqlite_cell(row, index)? {
        Cell::Text(text) => text.parse::<i64>().map_err(|_| format!("不是整数 {text}")),
        Cell::Null => Ok(0),
    }
}

/// 网格列的生成标记与默认值（R7）：table_xinfo 的 hidden 为 2/3 是生成列；dflt_value 是默认值原文，
/// 解析为 NULL 时不算默认值。
pub(crate) fn column_traits(hidden: i64, raw_default: Option<&str>) -> (bool, Option<String>) {
    let generated = hidden == 2 || hidden == 3;
    if generated {
        return (true, None);
    }
    let default = raw_default
        .filter(|raw| {
            !matches!(
                crate::ddl::sql::existing_default(Engine::Sqlite, Some(raw)),
                crate::ddl::DefaultSpec::None | crate::ddl::DefaultSpec::Null
            )
        })
        .map(str::to_owned);
    (false, default)
}
