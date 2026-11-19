//! SQLite 在已打开会话上的目录读取：库清单（main 与附加库）、对象分页、搜索与只读定义。

use sqlx::Row as _;

use crate::catalog::model::{kind_name, parse_kind, DatabaseNode, KindReport, ObjectKind, ObjectRow, SchemaNode};
use crate::catalog::read::{definition_failure, empty_page, read_failure, requested_limit, ConnectionBody, ObjectPageBody, ObjectWindow};

use super::catalog::{listed_kinds, quote_sqlite, sqlite_kind_count_sql, sqlite_kind_page_sql, sqlite_object_page_sql};

/// SQLite 的库清单来自 PRAGMA database_list：main 之后是每个 ATTACH 上来的库。
/// temp 不是用户库，不进树。
pub(super) async fn sqlite_database_names(pool: &sqlx::SqlitePool) -> Result<Vec<DatabaseNode>, String> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as("PRAGMA database_list").fetch_all(pool).await.map_err(read_failure)?;
    let databases: Vec<DatabaseNode> = rows
        .into_iter()
        .filter(|(_, name, _)| name != "temp")
        .map(|(_, name, _)| DatabaseNode {
            name,
            error: None,
            schemas: vec![SchemaNode::empty("")],
        })
        .collect();
    if databases.is_empty() {
        return Err(read_failure("没有读到 SQLite 数据库"));
    }
    Ok(databases)
}

pub(super) async fn read_sqlite(pool: &sqlx::SqlitePool, foreign_keys: Option<bool>, page: usize, windows: &[ObjectWindow]) -> ConnectionBody {
    let names = match sqlite_database_names(pool).await {
        Ok(names) => names,
        Err(message) => {
            return ConnectionBody {
                error: Some(message),
                foreign_keys,
                databases: Vec::new(),
            };
        }
    };
    let mut databases = Vec::new();
    for node in names {
        let limit = requested_limit(&node.name, "", windows, page);
        // 单个附加库读失败只让这个库带原因，其它库照常显示。
        databases.push(match read_sqlite_objects(pool, &node.name, 0, limit).await {
            Ok(schema) => DatabaseNode {
                name: node.name,
                error: None,
                schemas: vec![schema],
            },
            Err(message) => DatabaseNode {
                name: node.name,
                error: Some(message),
                schemas: vec![SchemaNode::empty("")],
            },
        });
    }
    ConnectionBody {
        error: None,
        foreign_keys,
        databases,
    }
}

/// 空库名按 main 处理：调用方不带库名时读的就是这条连接自己的库。
fn sqlite_database_or_main(database: &str) -> &str {
    if database.is_empty() {
        "main"
    } else {
        database
    }
}

fn object_from(name: String, kind: String, signature: String, relation: String) -> ObjectRow {
    ObjectRow {
        kind: parse_kind(&kind),
        name,
        estimated_rows: None,
        stale: false,
        signature,
        relation,
        materialized: false,
    }
}

/// 只读一种对象的一页：sqlite_master 按库分开，只读这一类的一页就够了。
pub(super) async fn read_kind_page(pool: &sqlx::SqlitePool, database: &str, kind: ObjectKind, offset: usize, limit: usize) -> ObjectPageBody {
    let database = sqlite_database_or_main(database);
    // 数量仍来自按种类的计数，分组的总数才和真实元数据一致。
    let counts: Result<Vec<(String, i64)>, String> =
        sqlx::query_as(sqlx::AssertSqlSafe(sqlite_kind_count_sql(database))).fetch_all(pool).await.map_err(read_failure);
    let total = match counts {
        Ok(counts) => counts
            .iter()
            .find(|(name, _)| parse_kind(name) == kind)
            .and_then(|(_, count)| usize::try_from(*count).ok())
            .unwrap_or(0),
        Err(message) => return empty_page(message),
    };
    let sql = sqlite_kind_page_sql(database, kind_name(kind));
    let rows: Result<Vec<(String, String, String, String)>, String> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .bind(i64::try_from(offset).unwrap_or(i64::MAX))
        .fetch_all(pool)
        .await
        .map_err(read_failure);
    match rows {
        Ok(rows) => {
            let objects = rows.into_iter().map(|(name, kind, signature, relation)| object_from(name, kind, signature, relation)).collect();
            ObjectPageBody {
                objects,
                total,
                error: None,
                kind_reports: Vec::new(),
            }
        }
        Err(message) => empty_page(message),
    }
}

pub(super) async fn read_sqlite_objects(pool: &sqlx::SqlitePool, database: &str, offset: usize, limit: usize) -> Result<SchemaNode, String> {
    let database = sqlite_database_or_main(database);
    // 库名只来自 PRAGMA database_list，并且按 SQL 标识符规则加引号；表名仍走绑定参数。
    let counts: Vec<(String, i64)> =
        sqlx::query_as(sqlx::AssertSqlSafe(sqlite_kind_count_sql(database))).fetch_all(pool).await.map_err(read_failure)?;
    let mut reports: Vec<KindReport> = listed_kinds()
        .iter()
        .copied()
        .map(|kind| KindReport {
            kind,
            total: 0,
            error: None,
        })
        .collect();
    for (kind_name, count) in counts {
        let kind = parse_kind(&kind_name);
        if let Some(report) = reports.iter_mut().find(|item| item.kind == kind) {
            report.total = usize::try_from(count).unwrap_or(0);
        }
    }
    let total = reports.iter().map(|item| item.total).sum();
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(sqlite_object_page_sql(database)))
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .bind(i64::try_from(offset).unwrap_or(i64::MAX))
        .fetch_all(pool)
        .await
        .map_err(read_failure)?;
    let objects = rows.into_iter().map(|(name, kind, signature, relation)| object_from(name, kind, signature, relation)).collect();
    Ok(SchemaNode {
        name: String::new(),
        objects,
        total,
        error: None,
        kind_reports: reports,
    })
}

/// 资源管理器全局搜索：逐个库（main 与附加库）检索表、视图与触发器的名字。
pub(super) async fn search(pool: &sqlx::SqlitePool, pattern: &str) -> Vec<(String, String, ObjectRow)> {
    const LIMIT: usize = 300;
    let Ok(dbs) = sqlite_database_names(pool).await else {
        return Vec::new();
    };
    let mut hits = Vec::new();
    for db in dbs {
        let db_name = sqlite_database_or_main(&db.name);
        let sql = format!(
            "SELECT name, type, '' AS sig, COALESCE(tbl_name, '') AS rel FROM {}.sqlite_master WHERE type IN ('table', 'view', 'trigger') AND name NOT LIKE 'sqlite_%' AND LOWER(name) LIKE LOWER(?1) ESCAPE '\\' ORDER BY name LIMIT ?2",
            quote_sqlite(db_name)
        );
        let rows: Result<Vec<(String, String, String, String)>, _> =
            sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(pattern).bind(LIMIT as i64).fetch_all(pool).await;
        if let Ok(rows) = rows {
            for (name, kind, signature, relation) in rows {
                hits.push((db.name.clone(), String::new(), object_from(name, kind, signature, relation)));
                if hits.len() >= LIMIT {
                    return hits;
                }
            }
        }
    }
    hits
}

pub(super) async fn read_sqlite_definition(pool: &sqlx::SqlitePool, database: &str, name: &str, kind: &str) -> Result<String, String> {
    let database = sqlite_database_or_main(database);
    if !matches!(kind, "trigger" | "table" | "view") {
        return Err(definition_failure(match kind {
            "function" | "procedure" => "SQLite 没有函数或存储过程",
            "sequence" | "enum" | "domain" => "SQLite 没有序列、枚举或域",
            _ => "只有表、视图和触发器可以读取只读定义",
        }));
    }
    // 附加库的对象在它自己的 sqlite_master 里，必须带上库名限定。表、视图、触发器都是建对象时保存的原文。
    let statement = format!("SELECT sql FROM {}.sqlite_master WHERE name = ?1 AND type = ?2", quote_sqlite(database));
    let sql: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(statement))
        .bind(name)
        .bind(kind)
        .fetch_optional(pool)
        .await
        .map_err(definition_failure)?;
    match sql {
        Some(text) if !text.trim().is_empty() => Ok(text),
        _ => Err(definition_failure("没有读到定义")),
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

/// 在目录会话上执行一条只读语句，把每格转成文本（对象详情与补全外键读取共用）。
pub(in crate::adapters) async fn text_rows(pool: &sqlx::SqlitePool, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
    use crate::catalog::detail::failure;
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql.to_owned())).fetch_all(pool).await.map_err(failure)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let width = row.columns().len();
            (0..width).map(|index| sqlite_row(&row, index)).collect()
        })
        .collect())
}
