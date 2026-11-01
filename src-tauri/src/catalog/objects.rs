//! 连接级对象详情（R5）：扩展、序列参数、事件、分区、PRAGMA 与附加库。
//!
//! 每节独立读取、独立报错：一节失败只让该节为空并带上原因，之前读到的树照原样保留。

use mysql_async::prelude::Queryable;

use crate::catalog::model::Engine;
use crate::connections::CatalogSession;

/// 一节结果：读到的条目和这一节自己的失败原因。失败不清空别的节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtrasList<T> {
    pub items: Vec<T>,
    pub error: Option<String>,
}

impl<T> Default for ExtrasList<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            error: None,
        }
    }
}

impl<T> ExtrasList<T> {
    fn failed(error: impl Into<String>) -> Self {
        Self {
            items: Vec::new(),
            error: Some(error.into()),
        }
    }
}

/// PostgreSQL 扩展：installed_version 为空表示可安装但还没装。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionRow {
    pub name: String,
    pub default_version: String,
    pub installed_version: Option<String>,
    pub comment: String,
}

/// 序列参数。数值列按 bigint 读，前端可据此生成 ALTER SEQUENCE。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceRow {
    pub schema: String,
    pub name: String,
    pub data_type: String,
    pub start_value: i64,
    pub min_value: i64,
    pub max_value: i64,
    pub increment: i64,
    pub cache_size: i64,
    pub cycle: bool,
    /// 从未取过值时留空。
    pub last_value: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub database: String,
    pub name: String,
    /// enabled / disabled / slaveside_disabled，information_schema.EVENTS.STATUS 的小写形式。
    pub status: String,
    /// 形如 EVERY 1 DAY 或 AT 2026-01-01 00:00:00；读不到时为空串。
    pub schedule: String,
    pub on_completion: String,
    pub definer: String,
    pub last_executed: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionItem {
    pub name: String,
    /// 范围/列表分区的边界描述，HASH/KEY 分区为空串。
    pub description: String,
    pub rows: Option<i64>,
}

/// 一张分区表的分区方式与分区清单（只读）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionRow {
    pub database: String,
    pub table: String,
    pub method: String,
    pub expression: String,
    pub partitions: Vec<PartitionItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PragmaRow {
    pub journal_mode: Option<String>,
    pub user_version: Option<i64>,
    pub foreign_keys: Option<bool>,
}

/// PRAGMA 一节：三者各自读取，失败只让对应字段留空。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PragmaSection {
    pub value: Option<PragmaRow>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedRow {
    pub name: String,
    pub file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExtrasBody {
    pub extensions: ExtrasList<ExtensionRow>,
    pub sequences: ExtrasList<SequenceRow>,
    pub events: ExtrasList<EventRow>,
    pub partitions: ExtrasList<PartitionRow>,
    pub pragmas: PragmaSection,
    pub attached: ExtrasList<AttachedRow>,
}

/// 读取范围：一个连接上具体某个库/schema 的对象详情。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtrasScope {
    pub connection_id: String,
    pub database: String,
    pub schema: String,
}

const POSTGRES_EXTENSIONS: &str = "SELECT a.name, a.default_version, a.installed_version, a.comment FROM pg_catalog.pg_available_extensions a ORDER BY a.name";

const POSTGRES_SEQUENCES: &str = "SELECT s.schemaname, s.sequencename, s.data_type::text, s.start_value, s.min_value, s.max_value, s.increment_by, s.cache_size, s.cycle, s.last_value FROM pg_catalog.pg_sequences s WHERE s.schemaname = $1 ORDER BY s.sequencename";

const POSTGRES_SEQUENCES_ALL: &str = "SELECT s.schemaname, s.sequencename, s.data_type::text, s.start_value, s.min_value, s.max_value, s.increment_by, s.cache_size, s.cycle, s.last_value FROM pg_catalog.pg_sequences s WHERE s.schemaname NOT IN ('pg_catalog', 'information_schema') ORDER BY s.schemaname, s.sequencename";

const MYSQL_EVENTS: &str = "SELECT EVENT_SCHEMA, EVENT_NAME, STATUS, DEFINER, CAST(EXECUTE_AT AS CHAR), INTERVAL_VALUE, INTERVAL_FIELD, ON_COMPLETION, CAST(LAST_EXECUTED AS CHAR) FROM information_schema.EVENTS WHERE EVENT_SCHEMA = ? ORDER BY EVENT_NAME";

const MYSQL_PARTITIONS: &str = "SELECT TABLE_SCHEMA, TABLE_NAME, PARTITION_NAME, PARTITION_METHOD, PARTITION_EXPRESSION, PARTITION_DESCRIPTION, TABLE_ROWS FROM information_schema.PARTITIONS WHERE TABLE_SCHEMA = ? AND PARTITION_NAME IS NOT NULL ORDER BY TABLE_NAME, PARTITION_ORDINAL_POSITION";

/// 一个原始分区行；分组后才会变成一张表的一条记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PartitionRaw {
    pub database: String,
    pub table: String,
    pub name: String,
    pub method: String,
    pub expression: String,
    pub description: String,
    pub rows: Option<i64>,
}

fn extras_failure(what: &str, err: impl std::fmt::Display) -> String {
    format!("读取{what}失败：{err}")
}

fn scripted_failure() -> String {
    "这个连接没有连着可查询的数据库".to_owned()
}

/// MySQL 事件的调度描述：优先周期，其次单次时间。
pub(crate) fn event_schedule(interval_value: &str, interval_field: &str, execute_at: &str) -> String {
    if !interval_value.is_empty() && !interval_field.is_empty() {
        format!("EVERY {interval_value} {interval_field}")
    } else if !execute_at.is_empty() {
        format!("AT {execute_at}")
    } else {
        String::new()
    }
}

/// 按表把分区行聚成一条记录，保持查询顺序（表名、分区序号）。
pub(crate) fn group_partitions(rows: Vec<PartitionRaw>) -> Vec<PartitionRow> {
    let mut grouped: Vec<PartitionRow> = Vec::new();
    for row in rows {
        let index = match grouped
            .iter()
            .position(|item| item.database == row.database && item.table == row.table)
        {
            Some(index) => index,
            None => {
                grouped.push(PartitionRow {
                    database: row.database.clone(),
                    table: row.table.clone(),
                    method: row.method.clone(),
                    expression: row.expression.clone(),
                    partitions: Vec::new(),
                });
                grouped.len() - 1
            }
        };
        grouped[index].partitions.push(PartitionItem {
            name: row.name,
            description: row.description,
            rows: row.rows,
        });
    }
    grouped
}

/// 三个 PRAGMA 各自成功或失败；只有失败的那个字段留空。
pub(crate) fn merge_pragmas(
    journal_mode: Result<String, String>,
    user_version: Result<i64, String>,
    foreign_keys: Result<i64, String>,
) -> PragmaSection {
    let mut errors = Vec::new();
    let journal_mode = match journal_mode {
        Ok(value) => Some(value),
        Err(message) => {
            errors.push(message);
            None
        }
    };
    let user_version = match user_version {
        Ok(value) => Some(value),
        Err(message) => {
            errors.push(message);
            None
        }
    };
    let foreign_keys = match foreign_keys {
        Ok(value) => Some(value != 0),
        Err(message) => {
            errors.push(message);
            None
        }
    };
    let value = if journal_mode.is_none() && user_version.is_none() && foreign_keys.is_none() {
        None
    } else {
        Some(PragmaRow {
            journal_mode,
            user_version,
            foreign_keys,
        })
    };
    PragmaSection {
        value,
        error: if errors.is_empty() { None } else { Some(errors.join("；")) },
    }
}

/// 读取一个连接上指定库/schema 的对象详情。不适用这一节的引擎返回空清单且不报错。
pub async fn read_extras(session: &CatalogSession, engine: Engine, database: &str, schema: &str) -> ExtrasBody {
    match session {
        CatalogSession::Scripted => ExtrasBody {
            extensions: ExtrasList::failed(scripted_failure()),
            sequences: ExtrasList::failed(scripted_failure()),
            events: ExtrasList::failed(scripted_failure()),
            partitions: ExtrasList::failed(scripted_failure()),
            pragmas: PragmaSection {
                value: None,
                error: Some(scripted_failure()),
            },
            attached: ExtrasList::failed(scripted_failure()),
        },
        CatalogSession::Sqlite { pool, .. } => read_sqlite_extras(pool).await,
        CatalogSession::Mysql(pool) => match engine {
            Engine::Mysql | Engine::Mariadb => read_mysql_extras(pool, database).await,
            _ => engine_without_extras(),
        },
        CatalogSession::Postgres(client) => match engine {
            Engine::Postgres => read_postgres_extras(client, schema).await,
            _ => engine_without_extras(),
        },
        CatalogSession::Sqlserver(_) | CatalogSession::Redis(_) | CatalogSession::Mongo(_) => engine_without_extras(),
    }
}

/// 这些引擎还没有对象详情读取：各节空清单，不报错也不编造。
fn engine_without_extras() -> ExtrasBody {
    ExtrasBody::default()
}

async fn read_postgres_extras(client: &tokio_postgres::Client, schema: &str) -> ExtrasBody {
    let mut body = ExtrasBody::default();
    match client.query(POSTGRES_EXTENSIONS, &[]).await {
        Ok(rows) => {
            body.extensions.items = rows
                .iter()
                .map(|row| ExtensionRow {
                    name: pg_text(row, 0),
                    default_version: pg_text(row, 1),
                    installed_version: pg_text_opt(row, 2),
                    comment: pg_text(row, 3),
                })
                .collect();
        }
        Err(err) => body.extensions.error = Some(extras_failure("扩展", err)),
    }
    let result = if schema.is_empty() {
        client.query(POSTGRES_SEQUENCES_ALL, &[]).await
    } else {
        let params: [&(dyn tokio_postgres::types::ToSql + Sync); 1] = [&schema];
        client.query(POSTGRES_SEQUENCES, &params).await
    };
    match result {
        Ok(rows) => {
            body.sequences.items = rows
                .iter()
                .map(|row| SequenceRow {
                    schema: pg_text(row, 0),
                    name: pg_text(row, 1),
                    data_type: pg_text(row, 2),
                    start_value: row.try_get::<_, i64>(3).unwrap_or_default(),
                    min_value: row.try_get::<_, i64>(4).unwrap_or_default(),
                    max_value: row.try_get::<_, i64>(5).unwrap_or_default(),
                    increment: row.try_get::<_, i64>(6).unwrap_or_default(),
                    cache_size: row.try_get::<_, i64>(7).unwrap_or_default(),
                    cycle: row.try_get::<_, bool>(8).unwrap_or(false),
                    last_value: row.try_get::<_, Option<i64>>(9).unwrap_or(None),
                })
                .collect();
        }
        Err(err) => body.sequences.error = Some(extras_failure("序列", err)),
    }
    body
}

async fn read_mysql_extras(pool: &mysql_async::Pool, database: &str) -> ExtrasBody {
    let mut body = ExtrasBody::default();
    if database.is_empty() {
        let message = "需要数据库名才能读取事件与分区".to_owned();
        body.events.error = Some(message.clone());
        body.partitions.error = Some(message);
        return body;
    }
    let mut conn = match pool.get_conn().await {
        Ok(conn) => conn,
        Err(err) => {
            let message = extras_failure("事件与分区", err);
            body.events.error = Some(message.clone());
            body.partitions.error = Some(message);
            return body;
        }
    };
    let events: Result<Vec<mysql_async::Row>, mysql_async::Error> =
        conn.exec(MYSQL_EVENTS, (database.to_owned(),)).await;
    match events {
        Ok(rows) => {
            body.events.items = rows
                .iter()
                .map(|row| {
                    let execute_at = mysql_text(row, 4);
                    let last_executed = mysql_text(row, 8);
                    EventRow {
                        database: mysql_text(row, 0),
                        name: mysql_text(row, 1),
                        status: mysql_text(row, 2).to_ascii_lowercase(),
                        definer: mysql_text(row, 3),
                        schedule: event_schedule(&mysql_text(row, 5), &mysql_text(row, 6), &execute_at),
                        on_completion: mysql_text(row, 7),
                        last_executed: if last_executed.is_empty() { None } else { Some(last_executed) },
                    }
                })
                .collect();
        }
        Err(err) => body.events.error = Some(extras_failure("事件", err)),
    }
    let partitions: Result<Vec<mysql_async::Row>, mysql_async::Error> =
        conn.exec(MYSQL_PARTITIONS, (database.to_owned(),)).await;
    match partitions {
        Ok(rows) => {
            let raw = rows
                .iter()
                .map(|row| PartitionRaw {
                    database: mysql_text(row, 0),
                    table: mysql_text(row, 1),
                    name: mysql_text(row, 2),
                    method: mysql_text(row, 3),
                    expression: mysql_text(row, 4),
                    description: mysql_text(row, 5),
                    rows: mysql_i64(row, 6),
                })
                .collect();
            body.partitions.items = group_partitions(raw);
        }
        Err(err) => body.partitions.error = Some(extras_failure("分区", err)),
    }
    body
}

async fn read_sqlite_extras(pool: &sqlx::SqlitePool) -> ExtrasBody {
    let journal_mode = sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
        .fetch_optional(pool)
        .await
        .map_err(|err| extras_failure("journal_mode", err))
        .and_then(|value| value.ok_or_else(|| extras_failure("journal_mode", "没有返回值")));
    let user_version = sqlx::query_scalar::<_, i64>("PRAGMA user_version")
        .fetch_optional(pool)
        .await
        .map_err(|err| extras_failure("user_version", err))
        .and_then(|value| value.ok_or_else(|| extras_failure("user_version", "没有返回值")));
    let foreign_keys = sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
        .fetch_optional(pool)
        .await
        .map_err(|err| extras_failure("foreign_keys", err))
        .and_then(|value| value.ok_or_else(|| extras_failure("foreign_keys", "没有返回值")));
    let mut body = ExtrasBody {
        pragmas: merge_pragmas(journal_mode, user_version, foreign_keys),
        ..ExtrasBody::default()
    };
    let attached: Result<Vec<(i64, String, String)>, sqlx::Error> =
        sqlx::query_as("PRAGMA database_list").fetch_all(pool).await;
    match attached {
        Ok(rows) => {
            body.attached.items = rows
                .into_iter()
                .map(|(_, name, file)| AttachedRow { name, file })
                .collect();
        }
        Err(err) => body.attached.error = Some(extras_failure("附加库", err)),
    }
    body
}

fn pg_text(row: &tokio_postgres::Row, index: usize) -> String {
    row.try_get::<_, String>(index)
        .or_else(|_| row.try_get::<_, Option<String>>(index).map(|value| value.unwrap_or_default()))
        .unwrap_or_default()
}

fn pg_text_opt(row: &tokio_postgres::Row, index: usize) -> Option<String> {
    row.try_get::<_, Option<String>>(index).ok().flatten()
}

fn mysql_value(row: &mysql_async::Row, index: usize) -> mysql_async::Value {
    row.as_ref(index).cloned().unwrap_or(mysql_async::Value::NULL)
}

fn mysql_text(row: &mysql_async::Row, index: usize) -> String {
    match mysql_value(row, index) {
        mysql_async::Value::Bytes(bytes) => String::from_utf8(bytes).unwrap_or_default(),
        mysql_async::Value::NULL => String::new(),
        other => other.as_sql(true),
    }
}

fn mysql_i64(row: &mysql_async::Row, index: usize) -> Option<i64> {
    match mysql_value(row, index) {
        mysql_async::Value::Int(value) => Some(value),
        mysql_async::Value::UInt(value) => i64::try_from(value).ok(),
        mysql_async::Value::Bytes(bytes) => std::str::from_utf8(&bytes).ok().and_then(|text| text.trim().parse().ok()),
        _ => None,
    }
}

#[cfg(test)]
mod extras_sql_tests {
    use super::{event_schedule, group_partitions, merge_pragmas, PartitionRaw};
    use super::{MYSQL_EVENTS, MYSQL_PARTITIONS, POSTGRES_EXTENSIONS, POSTGRES_SEQUENCES, POSTGRES_SEQUENCES_ALL};

    #[test]
    fn postgres_extras_read_available_extensions_and_sequence_parameters() {
        assert!(POSTGRES_EXTENSIONS.contains("pg_available_extensions"));
        assert!(POSTGRES_EXTENSIONS.contains("installed_version"));
        assert!(POSTGRES_SEQUENCES.contains("pg_sequences"));
        for column in [
            "start_value",
            "min_value",
            "max_value",
            "increment_by",
            "cache_size",
            "cycle",
        ] {
            assert!(POSTGRES_SEQUENCES.contains(column), "{column}");
        }
        assert!(POSTGRES_SEQUENCES.contains("schemaname = $1"));
        assert!(POSTGRES_SEQUENCES_ALL.contains("NOT IN ('pg_catalog', 'information_schema')"));
    }

    #[test]
    fn mysql_extras_read_events_and_partition_views() {
        assert!(MYSQL_EVENTS.contains("information_schema.EVENTS"));
        assert!(MYSQL_EVENTS.contains("EVENT_SCHEMA = ?"));
        assert!(MYSQL_EVENTS.contains("STATUS"));
        assert!(MYSQL_PARTITIONS.contains("information_schema.PARTITIONS"));
        assert!(MYSQL_PARTITIONS.contains("PARTITION_METHOD"));
        assert!(MYSQL_PARTITIONS.contains("PARTITION_NAME IS NOT NULL"));
        assert!(MYSQL_PARTITIONS.contains("PARTITION_ORDINAL_POSITION"));
    }

    #[test]
    fn event_schedule_prefers_the_recurring_interval() {
        assert_eq!(event_schedule("1", "DAY", ""), "EVERY 1 DAY");
        assert_eq!(event_schedule("", "", "2026-11-01 00:00:00"), "AT 2026-11-01 00:00:00");
        assert_eq!(event_schedule("", "", ""), "");
    }

    #[test]
    fn partitions_group_by_table_and_keep_the_query_order() {
        let raw = vec![
            partition("shop", "orders", "p0", "RANGE", "`created_at`", "2025-01-01", Some(10)),
            partition("shop", "orders", "p1", "RANGE", "`created_at`", "2026-01-01", None),
            partition("shop", "audit", "a0", "HASH", "`id`", "", Some(2)),
        ];
        let grouped = group_partitions(raw);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].table, "orders");
        assert_eq!(grouped[0].method, "RANGE");
        assert_eq!(grouped[0].partitions.len(), 2);
        assert_eq!(grouped[0].partitions[0].name, "p0");
        assert_eq!(grouped[0].partitions[0].description, "2025-01-01");
        assert_eq!(grouped[0].partitions[1].rows, None);
        assert_eq!(grouped[1].partitions[0].description, "");
    }

    #[test]
    fn one_failed_pragma_keeps_the_other_values() {
        let section = merge_pragmas(Ok("wal".to_owned()), Err("读取 user_version 失败：locked".to_owned()), Ok(0));
        let value = section.value.expect("其余两项仍然可读");
        assert_eq!(value.journal_mode.as_deref(), Some("wal"));
        assert_eq!(value.foreign_keys, Some(false));
        assert_eq!(value.user_version, None);
        assert!(section.error.as_deref().unwrap().contains("user_version"));

        let empty = merge_pragmas(Err("a".to_owned()), Err("b".to_owned()), Err("c".to_owned()));
        assert!(empty.value.is_none());
        assert_eq!(empty.error.as_deref(), Some("a；b；c"));
    }

    fn partition(
        database: &str,
        table: &str,
        name: &str,
        method: &str,
        expression: &str,
        description: &str,
        rows: Option<i64>,
    ) -> PartitionRaw {
        PartitionRaw {
            database: database.to_owned(),
            table: table.to_owned(),
            name: name.to_owned(),
            method: method.to_owned(),
            expression: expression.to_owned(),
            description: description.to_owned(),
            rows,
        }
    }
}
