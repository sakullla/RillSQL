//! MySQL 家族的连接级对象详情（R5）：事件与分区。每节独立读取、独立报错。

use mysql_async::prelude::Queryable;

use crate::catalog::objects::{event_schedule, extras_failure, group_partitions, EventRow, ExtrasBody, PartitionRaw};

pub(crate) const MYSQL_EVENTS: &str = "SELECT EVENT_SCHEMA, EVENT_NAME, STATUS, DEFINER, CAST(EXECUTE_AT AS CHAR), INTERVAL_VALUE, INTERVAL_FIELD, ON_COMPLETION, CAST(LAST_EXECUTED AS CHAR), EVENT_DEFINITION FROM information_schema.EVENTS WHERE EVENT_SCHEMA = ? ORDER BY EVENT_NAME";

pub(crate) const MYSQL_PARTITIONS: &str = "SELECT TABLE_SCHEMA, TABLE_NAME, PARTITION_NAME, PARTITION_METHOD, PARTITION_EXPRESSION, PARTITION_DESCRIPTION, TABLE_ROWS FROM information_schema.PARTITIONS WHERE TABLE_SCHEMA = ? AND PARTITION_NAME IS NOT NULL ORDER BY TABLE_NAME, PARTITION_ORDINAL_POSITION";

pub(super) async fn read_mysql_extras(pool: &mysql_async::Pool, database: &str) -> ExtrasBody {
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
    let events: Result<Vec<mysql_async::Row>, mysql_async::Error> = conn.exec(MYSQL_EVENTS, (database.to_owned(),)).await;
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
                        definition: mysql_text(row, 9),
                    }
                })
                .collect();
        }
        Err(err) => body.events.error = Some(extras_failure("事件", err)),
    }
    let partitions: Result<Vec<mysql_async::Row>, mysql_async::Error> = conn.exec(MYSQL_PARTITIONS, (database.to_owned(),)).await;
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
