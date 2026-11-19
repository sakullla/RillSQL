//! 对象概览统计（ADR-2 / R3）：连接级列库（或模式），库/模式级列表与视图，带表数量、估计行数、
//! 数据+索引大小、注释与更新时间。
//!
//! 这里是引擎无关的门面：统计 SQL 与行形状约定由各适配器的目录方言给出
//! （`CatalogDialect::{database_stats_sql, schema_stats_sql, object_stats_sql}`）。
//! 统计读不到（权限、版本、引擎不提供）时退回到已有的名称清单读取，保留名称并标注统计缺失。

use crate::adapters::{adapter, catalog_of};
use crate::catalog::model::{parse_kind, Engine, ObjectKind};
use crate::connections::CatalogSession;

/// 概览列的是哪一层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatsLevel {
    /// 连接级：库清单（名称、表数量、大小）。
    Databases,
    /// 库级（有模式层的引擎）：模式清单（名称、表数量、大小）。
    Schemas,
    /// 库/模式级：表与视图（名称、估计行数、注释、数据+索引大小、更新时间）。
    Objects,
}

/// 按引擎层级判断一个范围的概览列哪一层：没有库层的引擎在连接级直接列对象。
pub fn stats_level(engine: Engine, database: &str, schema: &str) -> StatsLevel {
    let capabilities = adapter(engine).capabilities();
    if database.trim().is_empty() && capabilities.shows_databases {
        StatsLevel::Databases
    } else if schema.trim().is_empty() && capabilities.shows_schemas {
        StatsLevel::Schemas
    } else {
        StatsLevel::Objects
    }
}

/// 概览里的一行。读不到的统计为 None / 空串，不编造。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatRow {
    pub name: String,
    /// 库/模式行为 None；对象行为表或视图。
    pub kind: Option<ObjectKind>,
    /// 库/模式里的表数量。
    pub tables: Option<u64>,
    /// 估计行数（统计信息，不是 COUNT(*)）。
    pub rows: Option<u64>,
    /// 数据+索引大小（字节）。
    pub size: Option<u64>,
    pub comment: String,
    /// 最近更新时间（`YYYY-MM-DD HH:MM:SS`）；引擎不记录时为空。
    pub updated: String,
}

impl StatRow {
    pub fn named(name: impl Into<String>, kind: Option<ObjectKind>) -> Self {
        Self { name: name.into(), kind, tables: None, rows: None, size: None, comment: String::new(), updated: String::new() }
    }
}

/// 一次概览读取的结果。`missing` 有值时 rows 只有名称（统计读取失败的原因）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsBody {
    pub level: StatsLevel,
    pub rows: Vec<StatRow>,
    pub missing: Option<String>,
}

/// 没有库层的引擎在连接级列对象：落到它的主库。
fn scope_database(engine: Engine, level: StatsLevel, database: &str) -> &str {
    match (level, database.trim().is_empty()) {
        (StatsLevel::Objects, true) => catalog_of(engine).default_database().unwrap_or(database),
        _ => database,
    }
}

/// 名称清单退回读取的上限：概览本地过滤与排序，一次读完常见规模的库。
pub const FALLBACK_LIMIT: usize = 10_000;

/// 这一层的统计 SQL（已按目标库限定）。None 表示引擎不提供这一层的统计。
pub fn stats_sql(engine: Engine, level: StatsLevel, database: &str, schema: &str) -> Option<String> {
    let dialect = catalog_of(engine);
    let database = scope_database(engine, level, database);
    let sql = match level {
        StatsLevel::Databases => dialect.database_stats_sql()?,
        StatsLevel::Schemas => dialect.schema_stats_sql(database)?,
        StatsLevel::Objects => dialect.object_stats_sql(database, schema)?,
    };
    Some(match level {
        StatsLevel::Databases => sql,
        StatsLevel::Schemas | StatsLevel::Objects => dialect.scope_sql(database, &sql),
    })
}

fn cell(rows: &[Vec<Option<String>>], row: usize, column: usize) -> String {
    crate::adapters::catalog::cell(rows, row, column)
}

/// 统计数字：小数（MySQL SUM 的 DECIMAL、PG reltuples 的浮点）取整，负数（未知估算）和空值为 None。
fn number(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(value) = text.parse::<i128>() {
        return u64::try_from(value).ok();
    }
    let value = text.parse::<f64>().ok()?;
    (value.is_finite() && value >= 0.0).then(|| value.round() as u64)
}

/// 统计查询结果收成行。库/模式行：name, tables, size；对象行：name, kind, rows, size, comment, updated。
pub fn rows_from_stats(level: StatsLevel, rows: &[Vec<Option<String>>]) -> Vec<StatRow> {
    (0..rows.len())
        .filter_map(|index| {
            let name = cell(rows, index, 0);
            if name.is_empty() {
                return None;
            }
            Some(match level {
                StatsLevel::Databases | StatsLevel::Schemas => StatRow {
                    tables: number(&cell(rows, index, 1)),
                    size: number(&cell(rows, index, 2)),
                    ..StatRow::named(name, None)
                },
                StatsLevel::Objects => StatRow {
                    kind: Some(parse_kind(cell(rows, index, 1).trim())),
                    rows: number(&cell(rows, index, 2)),
                    size: number(&cell(rows, index, 3)),
                    comment: cell(rows, index, 4).trim().to_owned(),
                    updated: cell(rows, index, 5).trim().to_owned(),
                    name,
                    tables: None,
                },
            })
        })
        .collect()
}

/// 读一个范围的概览：先读统计，失败或不提供时退回名称清单并标注统计缺失。名称也读不到才算失败。
pub async fn read_object_stats(engine: Engine, session: &CatalogSession, database: &str, schema: &str) -> Result<StatsBody, String> {
    let level = stats_level(engine, database, schema);
    let sql = stats_sql(engine, level, database, schema);
    read_stats_with(engine, session, level, database, schema, sql).await
}

/// 按给定的统计语句读取；测试用它模拟统计失败。
pub(crate) async fn read_stats_with(
    engine: Engine,
    session: &CatalogSession,
    level: StatsLevel,
    database: &str,
    schema: &str,
    sql: Option<String>,
) -> Result<StatsBody, String> {
    let database = scope_database(engine, level, database);
    let missing = match sql {
        Some(sql) => match session.text_rows(&sql).await {
            Ok(rows) => return Ok(StatsBody { level, rows: rows_from_stats(level, &rows), missing: None }),
            Err(error) => error,
        },
        None => "catalog.overview.statsUnsupported".to_owned(),
    };
    let rows = read_names(engine, session, level, database, schema).await.map_err(|names| if names == missing { names } else { format!("{missing}\n{names}") })?;
    Ok(StatsBody { level, rows, missing: Some(missing) })
}

/// 统计读不到时的名称清单：沿用导航的库/模式/分类分页读取。
async fn read_names(engine: Engine, session: &CatalogSession, level: StatsLevel, database: &str, schema: &str) -> Result<Vec<StatRow>, String> {
    let dialect = catalog_of(engine);
    match level {
        StatsLevel::Databases => {
            let body = dialect.read_outline(session).await;
            if body.databases.is_empty() {
                if let Some(error) = body.error {
                    return Err(error);
                }
            }
            Ok(body.databases.into_iter().map(|database| StatRow::named(database.name, None)).collect())
        }
        StatsLevel::Schemas => {
            let schemas = match dialect.read_database_outline(session, database).await {
                Ok(schemas) => schemas,
                // 会话当前库的模式随连接概要一起读出，不再按库单读。
                Err(error) => {
                    let body = dialect.read_outline(session).await;
                    match body.databases.into_iter().find(|item| item.name == database) {
                        Some(found) => found.schemas,
                        None => return Err(error),
                    }
                }
            };
            Ok(schemas.into_iter().filter(|schema| !schema.name.is_empty()).map(|schema| StatRow::named(schema.name, None)).collect())
        }
        StatsLevel::Objects => {
            let mut rows = Vec::new();
            let mut failure = None;
            for kind in [ObjectKind::Table, ObjectKind::View] {
                if !dialect.listed_kinds().contains(&kind) {
                    continue;
                }
                let page = dialect.read_kind_page(session, database, schema, kind, 0, FALLBACK_LIMIT).await;
                if let Some(error) = page.error.filter(|_| page.objects.is_empty()) {
                    failure = Some(error);
                    continue;
                }
                rows.extend(page.objects.into_iter().filter(|object| object.kind == kind).map(|object| StatRow {
                    rows: object.estimated_rows.and_then(|value| u64::try_from(value).ok()),
                    ..StatRow::named(object.name, Some(kind))
                }));
            }
            match failure {
                Some(error) if rows.is_empty() => Err(error),
                _ => {
                    rows.sort_by(|left, right| left.name.cmp(&right.name));
                    Ok(rows)
                }
            }
        }
    }
}
