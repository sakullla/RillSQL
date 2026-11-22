//! 连接级对象详情（R5）：扩展、序列参数、事件、分区、PRAGMA 与附加库。
//!
//! 每节独立读取、独立报错：一节失败只让该节为空并带上原因，之前读到的树照原样保留。
//! 各引擎的读取与 SQL 在 `adapters/<engine>/extras.rs`；这里是数据形状、引擎无关的整理规则与门面。

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
    /// information_schema.EVENTS.EVENT_DEFINITION：事件体原文，面板按需展开只读查看。
    pub definition: String,
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

pub(crate) fn extras_failure(what: &str, err: impl Into<crate::error_text::Reason>) -> String {
    let err = err.into();
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

/// 对象编辑器读到的单个对象状态：事件、例程或触发器。三者形状不同，各用自己的快照。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObjectDetail {
    Event(crate::ddl::model::EventState),
    Routine(crate::ddl::model::RoutineState),
    Trigger(crate::ddl::model::TriggerState),
}

impl ObjectDetail {
    /// 读回来的对象名，用于核对"改的还是打开的那个对象"。
    pub fn name(&self) -> &str {
        match self {
            ObjectDetail::Event(state) => &state.name,
            ObjectDetail::Routine(state) => &state.name,
            ObjectDetail::Trigger(state) => &state.name,
        }
    }

    /// 事件调度器状态：只有事件有；其余对象为 None（未知不等于"会运行"）。
    pub fn scheduler_running(&self) -> Option<bool> {
        match self {
            ObjectDetail::Event(state) => state.scheduler_running,
            _ => None,
        }
    }
}

/// 读一个事件/例程/触发器的完整状态（对象编辑器的打开与失败后读回都走这里）。
///
/// 只有 MySQL 与 MariaDB 有这些对象；其它引擎给出原因，不编造空对象。读取失败只让这一个对象读不到，
/// 不影响对象树、普通表结构或其它对象分组。
pub async fn read_object_detail(
    session: &CatalogSession,
    engine: Engine,
    database: &str,
    kind: crate::catalog::model::ObjectKind,
    name: &str,
) -> Result<ObjectDetail, String> {
    if session.is_scripted() {
        return Err(scripted_failure());
    }
    if database.trim().is_empty() {
        return Err("需要数据库名才能读取这个对象。".to_owned());
    }
    crate::adapters::mysql::catalog_read::read_object_detail_session(session, engine, database, kind, name).await
}

/// 读取一个连接上指定库/schema 的对象详情。不适用这一节的引擎返回空清单且不报错，不编造。
pub async fn read_extras(session: &CatalogSession, engine: Engine, database: &str, schema: &str) -> ExtrasBody {
    if session.is_scripted() {
        return ExtrasBody {
            extensions: ExtrasList::failed(scripted_failure()),
            sequences: ExtrasList::failed(scripted_failure()),
            events: ExtrasList::failed(scripted_failure()),
            partitions: ExtrasList::failed(scripted_failure()),
            pragmas: PragmaSection {
                value: None,
                error: Some(scripted_failure()),
            },
            attached: ExtrasList::failed(scripted_failure()),
        };
    }
    crate::adapters::catalog_of(engine).read_extras(session, database, schema).await
}

#[cfg(test)]
mod extras_sql_tests {
    use super::{event_schedule, group_partitions, merge_pragmas, PartitionRaw};
    use crate::adapters::mysql::extras::{MYSQL_EVENTS, MYSQL_PARTITIONS};
    use crate::adapters::postgres::extras::{POSTGRES_EXTENSIONS, POSTGRES_SEQUENCES, POSTGRES_SEQUENCES_ALL};

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
        // 事件体只读展示用这一列；位置固定在最后，前面各列索引不变。
        assert!(MYSQL_EVENTS.contains("EVENT_DEFINITION"));
        assert!(MYSQL_EVENTS.ends_with("EVENT_DEFINITION FROM information_schema.EVENTS WHERE EVENT_SCHEMA = ? ORDER BY EVENT_NAME"));
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