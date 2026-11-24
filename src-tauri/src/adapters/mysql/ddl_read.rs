//! MySQL 家族的结构内省与执行：information_schema 与 SHOW CREATE 读表结构，逐条执行（每条结构语句立即提交）。

use crate::adapters::quote_ident;
use crate::connections::model::Engine;
use crate::ddl::exec::receipt;
use crate::ddl::introspect::keep_rows;
use crate::ddl::model::{ForeignKey, IncomingForeignKey, Index, IndexColumn, IndexKind, Snapshot, Step, StepReceipt, StepStatus};
use crate::ddl::sql::qualified_table;

/// 改名前查新名字：MySQL 看同库的表与视图（名字逐字节比较，排除被改名的表自身）。
pub(super) async fn name_taken(pool: &mysql_async::Pool, database: &str, current: &str, name: &str) -> Result<bool, String> {
    use mysql_async::prelude::Queryable;
    let mut conn = pool.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
    let count: Option<i64> = conn
        .exec_first(
            "SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND BINARY TABLE_NAME <> BINARY ?",
            (database, name, current),
        )
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    Ok(count.unwrap_or(0) > 0)
}

pub(super) async fn count(pool: &mysql_async::Pool, sql: &str) -> Result<i64, String> {
    let mut conn = pool.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
    let row: Option<mysql_async::Row> =
        mysql_async::prelude::Queryable::query_first(&mut conn, sql.to_owned()).await.map_err(|err| crate::error_text::chain(&err))?;
    let row = row.ok_or_else(|| "没有读到 NULL 计数".to_owned())?;
    Ok(mysql_i64(&row, 0))
}

pub(super) async fn mysql_event_enabled(pool: &mysql_async::Pool, database: &str, event: &str) -> Result<Option<bool>, String> {
    if database.is_empty() {
        return Err("需要数据库名，没有另开连接。".to_owned());
    }
    let mut conn = pool.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
    let row: Option<mysql_async::Row> = mysql_async::prelude::Queryable::exec_first(
        &mut conn,
        "SELECT STATUS FROM information_schema.EVENTS WHERE EVENT_SCHEMA = ? AND EVENT_NAME = ?",
        (database.to_owned(), event.to_owned()),
    )
    .await
    .map_err(|err| format!("读不到事件 {event}：{err}"))?;
    let Some(row) = row else {
        return Err(format!("找不到事件 {database}.{event}。"));
    };
    Ok(Some(mysql_string(&row, 0).eq_ignore_ascii_case("ENABLED")))
}

pub(super) async fn load_mysql(pool: &mysql_async::Pool, engine: Engine, database: &str, table: &str) -> Result<Snapshot, String> {
    if database.is_empty() {
        return Err("需要数据库名，没有另开连接。".to_owned());
    }
    let mut conn = pool.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
    let kind: Option<mysql_async::Row> = mysql_async::prelude::Queryable::exec_first(
        &mut conn,
        "SELECT TABLE_TYPE, ENGINE FROM information_schema.tables WHERE table_schema = ? AND table_name = ?",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(|err| crate::error_text::chain(&err))?;
    let Some(kind_row) = kind else {
        return Err(format!("找不到表或视图 {database}.{table}"));
    };
    let mut snapshot = Snapshot::bare(engine, database, database, table);
    snapshot.kind = if mysql_string(&kind_row, 0).eq_ignore_ascii_case("VIEW") { "view" } else { "table" }.to_owned();
    snapshot.storage = Some(mysql_string(&kind_row, 1));
    // 版本与 sql_mode 决定方言能力与字面量转义。读不到时只记原因：表结构本身仍然可用。
    match super::profile::read_profile_from(&mut conn, engine).await {
        Ok(profile) => {
            snapshot.server_version = profile.raw_version.clone();
            snapshot.sql_mode = profile.sql_mode.clone();
        }
        Err(message) => snapshot.profile_error = Some(message),
    }
    let columns: Vec<mysql_async::Row> = mysql_async::prelude::Queryable::exec(
        &mut conn,
        super::table_read::COLUMNS_SQL,
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(|err| crate::error_text::chain(&err))?;
    for row in &columns {
        super::table_read::push_column(&mut snapshot, row, 0);
    }
    // 分区读取失败只影响分区：字段、索引与外键照常可用。
    match super::table_read::load_partitions(&mut conn, database, table).await {
        Ok(partitions) => snapshot.partitions = partitions,
        Err(message) => snapshot.partition_error = Some(message),
    }
    let visibility = statistics_visibility(&mut conn).await;
    let indexes: Vec<mysql_async::Row> = keep_rows(
        &mut snapshot.index_error,
        mysql_async::prelude::Queryable::exec(
            &mut conn,
            format!(
                "SELECT INDEX_NAME, NON_UNIQUE, COLUMN_NAME, COLLATION, INDEX_TYPE, SUB_PART, INDEX_COMMENT, {} FROM information_schema.statistics WHERE table_schema = ? AND table_name = ? ORDER BY INDEX_NAME, SEQ_IN_INDEX",
                visibility.select_expr()
            ),
            (database.to_owned(), table.to_owned()),
        )
        .await
        .map_err(|err| crate::error_text::chain(&err)),
    );
    for row in &indexes {
        push_mysql_index(&mut snapshot.indexes, row, 0);
    }
    mark_mysql_primary(&mut snapshot);
    let foreign_keys: Vec<mysql_async::Row> = keep_rows(
        &mut snapshot.foreign_key_error,
        mysql_async::prelude::Queryable::exec(
            &mut conn,
            "SELECT k.CONSTRAINT_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_NAME, k.REFERENCED_COLUMN_NAME, r.DELETE_RULE, r.UPDATE_RULE FROM information_schema.KEY_COLUMN_USAGE k JOIN information_schema.REFERENTIAL_CONSTRAINTS r ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME AND r.TABLE_NAME = k.TABLE_NAME WHERE k.TABLE_SCHEMA = ? AND k.TABLE_NAME = ? AND k.REFERENCED_TABLE_NAME IS NOT NULL ORDER BY k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
            (database.to_owned(), table.to_owned()),
        )
        .await
        .map_err(|err| crate::error_text::chain(&err)),
    );
    if snapshot.foreign_key_error.is_none() {
        push_mysql_fks(&mut snapshot.foreign_keys, &foreign_keys, false);
    }
    protect_mysql_fk_indexes(&mut snapshot);
    let incoming: Vec<mysql_async::Row> = keep_rows(
        &mut snapshot.foreign_key_error,
        mysql_async::prelude::Queryable::exec(
            &mut conn,
            "SELECT k.CONSTRAINT_NAME, k.TABLE_NAME, k.COLUMN_NAME, k.REFERENCED_COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE k WHERE k.REFERENCED_TABLE_SCHEMA = ? AND k.REFERENCED_TABLE_NAME = ? AND k.REFERENCED_COLUMN_NAME IS NOT NULL ORDER BY k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
            (database.to_owned(), table.to_owned()),
        )
        .await
        .map_err(|err| crate::error_text::chain(&err)),
    );
    let mut incoming_keys = Vec::new();
    for row in &incoming {
        let name = mysql_string(row, 0);
        let child = mysql_string(row, 1);
        let column = mysql_string(row, 2);
        let referenced = mysql_string(row, 3);
        if let Some(key) = incoming_keys.iter_mut().find(|key: &&mut IncomingForeignKey| key.name == name && key.table == child) {
            key.columns.push(column);
            key.ref_columns.push(referenced);
        } else {
            incoming_keys.push(IncomingForeignKey { name, table: child, schema: database.to_owned(), columns: vec![column], ref_columns: vec![referenced] });
        }
    }
    snapshot.incoming = incoming_keys;
    snapshot.row_count = Some(count_nulls_mysql_count(&mut conn, database, table).await?);
    snapshot.object_exists = true;
    if snapshot.kind == "view" {
        match mysql_view_definition(&mut conn, database, table).await {
            Ok(sql) => snapshot.table_sql = sql,
            Err(message) => {
                snapshot.table_sql.clear();
                snapshot.definition_error = Some(message);
            }
        }
    } else {
        // 表的 DDL 用服务器自己的 SHOW CREATE TABLE 原文；读不到时退回占位，DDL 页签改用重建定义。
        snapshot.table_sql = match mysql_table_definition(&mut conn, database, table).await {
            Ok(sql) => sql,
            Err(_) => format!("-- {}.{table}", database),
        };
    }
    Ok(snapshot)
}

/// information_schema.statistics 里表示索引可见性的列：MySQL 8.0+ 是 IS_VISIBLE，MariaDB 10.6+ 是 IGNORED（含义相反），
/// 更早的版本都没有。按列是否存在选择，不按版本号猜。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatisticsVisibility {
    IsVisible,
    Ignored,
    Missing,
}

impl StatisticsVisibility {
    /// 选出 'YES'（可见）/'NO'（不可见）/NULL（没有这项）的表达式。
    pub(crate) fn select_expr(self) -> &'static str {
        match self {
            StatisticsVisibility::IsVisible => "IS_VISIBLE",
            StatisticsVisibility::Ignored => "CASE WHEN IGNORED = 'YES' THEN 'NO' ELSE 'YES' END",
            StatisticsVisibility::Missing => "NULL",
        }
    }

    pub(crate) fn of_columns<S: AsRef<str>>(names: &[S]) -> Self {
        if names.iter().any(|name| name.as_ref().eq_ignore_ascii_case("IS_VISIBLE")) {
            StatisticsVisibility::IsVisible
        } else if names.iter().any(|name| name.as_ref().eq_ignore_ascii_case("IGNORED")) {
            StatisticsVisibility::Ignored
        } else {
            StatisticsVisibility::Missing
        }
    }
}

/// 读不到列清单时按没有可见性处理：索引其余信息照常读。
async fn statistics_visibility(conn: &mut mysql_async::Conn) -> StatisticsVisibility {
    let names: Result<Vec<String>, _> = mysql_async::prelude::Queryable::query(
        conn,
        "SELECT COLUMN_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = 'information_schema' AND TABLE_NAME = 'STATISTICS' AND COLUMN_NAME IN ('IS_VISIBLE', 'IGNORED')",
    )
    .await;
    names.map(|names| StatisticsVisibility::of_columns(&names)).unwrap_or(StatisticsVisibility::Missing)
}

/// INDEX_TYPE 与 NON_UNIQUE 推出的索引类型。主键按名字 PRIMARY 认。
pub(crate) fn mysql_index_kind(name: &str, non_unique: &str, index_type: &str) -> IndexKind {
    if name.eq_ignore_ascii_case("PRIMARY") {
        IndexKind::Primary
    } else if index_type.eq_ignore_ascii_case("FULLTEXT") {
        IndexKind::Fulltext
    } else if index_type.eq_ignore_ascii_case("SPATIAL") {
        IndexKind::Spatial
    } else if non_unique == "0" {
        IndexKind::Unique
    } else {
        IndexKind::Normal
    }
}

/// 'YES'/'NO'/空 换成可见性。
pub(crate) fn mysql_visibility(text: &str) -> Option<bool> {
    match text.trim().to_ascii_uppercase().as_str() {
        "YES" => Some(true),
        "NO" => Some(false),
        _ => None,
    }
}

/// statistics 的一行（INDEX_NAME, NON_UNIQUE, COLUMN_NAME, COLLATION, INDEX_TYPE, SUB_PART, INDEX_COMMENT, 可见性 从 `at` 起）并入索引列表。
fn push_mysql_index(indexes: &mut Vec<Index>, row: &mysql_async::Row, at: usize) {
    let name = mysql_string(row, at);
    let column_name = mysql_string(row, at + 2);
    // 8.0.13+ 的函数索引键：COLUMN_NAME 为 NULL（表达式在 EXPRESSION 列），按表达式索引处理，不当成空列名。
    let functional = column_name.is_empty();
    let column = IndexColumn {
        name: column_name,
        descending: mysql_string(row, at + 3).eq_ignore_ascii_case("D"),
        prefix: mysql_string(row, at + 5).trim().parse::<u32>().ok().filter(|length| *length > 0),
    };
    if let Some(index) = indexes.iter_mut().find(|index| index.name == name) {
        if functional {
            index.expression = true;
        } else {
            index.columns.push(column);
        }
    } else {
        let primary = name.eq_ignore_ascii_case("PRIMARY");
        let non_unique = mysql_string(row, at + 1);
        let method = mysql_string(row, at + 4);
        let comment = mysql_string(row, at + 6);
        indexes.push(Index {
            kind: mysql_index_kind(&name, &non_unique, &method),
            name,
            unique: non_unique == "0",
            columns: if functional { Vec::new() } else { vec![column] },
            protected: primary,
            reason: if primary { "由主键约束维护".to_owned() } else { String::new() },
            partial: false,
            expression: functional,
            sql: None,
            method: Some(method).filter(|text| !text.trim().is_empty()),
            comment: Some(comment).filter(|text| !text.trim().is_empty()),
            visible: mysql_visibility(&mysql_string(row, at + 7)),
            rebuild_loses: Vec::new(),
        });
    }
}

fn mark_mysql_primary(snapshot: &mut Snapshot) {
    for column in &mut snapshot.columns {
        column.primary = snapshot.indexes.iter().any(|index| index.name.eq_ignore_ascii_case("PRIMARY") && index.columns.iter().any(|item| item.name == column.name));
    }
}

fn protect_mysql_fk_indexes(snapshot: &mut Snapshot) {
    for key in &snapshot.foreign_keys {
        if let Some(index) = snapshot.indexes.iter_mut().find(|index| index.name == key.name) {
            index.protected = true;
            index.reason = format!("外键 {} 依赖此索引", key.name);
        }
    }
}

/// 对比用：一个库里全部基本表的列、索引与外键，三条查询读完（逐表读取每张表要七八次往返）。
/// 只填对比用到的部分；行数、入向外键与建表原文不读。
pub(super) async fn load_mysql_tables(pool: &mysql_async::Pool, engine: Engine, database: &str) -> Result<Vec<(String, Snapshot)>, String> {
    use mysql_async::prelude::Queryable;
    if database.is_empty() {
        return Err("需要数据库名，没有另开连接。".to_owned());
    }
    let mut conn = pool.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
    let tables: Vec<mysql_async::Row> = conn
        .exec(
            "SELECT TABLE_NAME FROM information_schema.tables WHERE table_schema = ? AND TABLE_TYPE = 'BASE TABLE' ORDER BY TABLE_NAME",
            (database.to_owned(),),
        )
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    let mut snapshots: std::collections::BTreeMap<String, Snapshot> = std::collections::BTreeMap::new();
    for row in &tables {
        let name = mysql_string(row, 0);
        let mut snapshot = Snapshot::bare(engine, database, database, &name);
        snapshot.table_sql = String::new();
        snapshot.row_count = None;
        snapshots.insert(name, snapshot);
    }
    let columns: Vec<mysql_async::Row> = conn
        .exec(
            "SELECT c.TABLE_NAME, c.COLUMN_NAME, c.COLUMN_TYPE, c.IS_NULLABLE, c.COLUMN_DEFAULT, c.EXTRA, c.GENERATION_EXPRESSION FROM information_schema.columns c JOIN information_schema.tables t ON t.TABLE_SCHEMA = c.TABLE_SCHEMA AND t.TABLE_NAME = c.TABLE_NAME AND t.TABLE_TYPE = 'BASE TABLE' WHERE c.table_schema = ? ORDER BY c.TABLE_NAME, c.ORDINAL_POSITION",
            (database.to_owned(),),
        )
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    for row in &columns {
        if let Some(snapshot) = snapshots.get_mut(&mysql_string(row, 0)) {
            // 批量对比路径只要列本身；ENUM/SET 候选值与生成列不参与对比。
            let read = super::table_read::column(snapshot.literal_quoting(), row, 1);
            snapshot.columns.push(read.column);
        }
    }
    let mut index_error = None;
    // 与逐表读取同一组字段（含可见性），对比时两条路径读到的索引一致。
    let visibility = statistics_visibility(&mut conn).await;
    let indexes = keep_rows(
        &mut index_error,
        conn.exec(
            format!(
                "SELECT TABLE_NAME, INDEX_NAME, NON_UNIQUE, COLUMN_NAME, COLLATION, INDEX_TYPE, SUB_PART, INDEX_COMMENT, {} FROM information_schema.statistics WHERE table_schema = ? ORDER BY TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX",
                visibility.select_expr()
            ),
            (database.to_owned(),),
        )
        .await
        .map_err(|err| crate::error_text::chain(&err)),
    );
    for row in &indexes {
        if let Some(snapshot) = snapshots.get_mut(&mysql_string(row, 0)) {
            push_mysql_index(&mut snapshot.indexes, row, 1);
        }
    }
    let mut foreign_key_error = None;
    let foreign_keys: Vec<mysql_async::Row> = keep_rows(
        &mut foreign_key_error,
        conn.exec(
            "SELECT k.TABLE_NAME, k.CONSTRAINT_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_NAME, k.REFERENCED_COLUMN_NAME, r.DELETE_RULE, r.UPDATE_RULE FROM information_schema.KEY_COLUMN_USAGE k JOIN information_schema.REFERENTIAL_CONSTRAINTS r ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME AND r.TABLE_NAME = k.TABLE_NAME WHERE k.TABLE_SCHEMA = ? AND k.REFERENCED_TABLE_NAME IS NOT NULL ORDER BY k.TABLE_NAME, k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
            (database.to_owned(),),
        )
        .await
        .map_err(|err| crate::error_text::chain(&err)),
    );
    let mut grouped: std::collections::BTreeMap<String, Vec<mysql_async::Row>> = std::collections::BTreeMap::new();
    for row in foreign_keys {
        grouped.entry(mysql_string(&row, 0)).or_default().push(row);
    }
    let mut out = Vec::with_capacity(snapshots.len());
    for (name, mut snapshot) in snapshots {
        snapshot.index_error = index_error.clone();
        snapshot.foreign_key_error = foreign_key_error.clone();
        mark_mysql_primary(&mut snapshot);
        if snapshot.foreign_key_error.is_none() {
            let rows = grouped.remove(&name).unwrap_or_default();
            push_mysql_fks_at(&mut snapshot.foreign_keys, &rows, 1);
        }
        protect_mysql_fk_indexes(&mut snapshot);
        out.push((name, snapshot));
    }
    Ok(out)
}

async fn mysql_table_definition(conn: &mut mysql_async::Conn, database: &str, table: &str) -> Result<String, String> {
    let sql = format!(
        "SHOW CREATE TABLE {}.{}",
        quote_ident(Engine::Mysql, database),
        quote_ident(Engine::Mysql, table)
    );
    let row: Option<mysql_async::Row> = mysql_async::prelude::Queryable::query_first(conn, sql).await.map_err(|err| crate::error_text::chain(&err))?;
    let row = row.ok_or_else(|| "没有读到建表语句".to_owned())?;
    let text = mysql_string(&row, 1);
    if !text.trim_start().to_ascii_uppercase().starts_with("CREATE") {
        return Err("没有读到建表语句".to_owned());
    }
    Ok(format!("{};", text.trim_end().trim_end_matches(';')))
}

async fn mysql_view_definition(conn: &mut mysql_async::Conn, database: &str, table: &str) -> Result<String, String> {
    let sql = format!(
        "SHOW CREATE VIEW {}.{}",
        quote_ident(Engine::Mysql, database),
        quote_ident(Engine::Mysql, table)
    );
    let row: Option<mysql_async::Row> = mysql_async::prelude::Queryable::query_first(conn, sql).await.map_err(|err| crate::error_text::chain(&err))?;
    let row = row.ok_or_else(|| "没有读到视图定义".to_owned())?;
    let text = mysql_string(&row, 1);
    if text.trim().is_empty() || text.trim().starts_with("--") {
        return Err("没有读到视图定义".to_owned());
    }
    Ok(text)
}

fn push_mysql_fks(target: &mut Vec<ForeignKey>, rows: &[mysql_async::Row], _incoming: bool) {
    push_mysql_fks_at(target, rows, 0);
}

/// 外键行（CONSTRAINT_NAME, COLUMN_NAME, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME, DELETE_RULE, UPDATE_RULE 从 `at` 起）。
fn push_mysql_fks_at(target: &mut Vec<ForeignKey>, rows: &[mysql_async::Row], at: usize) {
    for row in rows {
        let name = mysql_string(row, at);
        let column = mysql_string(row, at + 1);
        let ref_table = mysql_string(row, at + 2);
        let ref_column = mysql_string(row, at + 3);
        let on_delete = mysql_string(row, at + 4);
        let on_update = mysql_string(row, at + 5);
        if let Some(key) = target.iter_mut().find(|key| key.name == name) {
            key.columns.push(column);
            key.ref_columns.push(ref_column);
        } else {
            target.push(ForeignKey {
                name,
                columns: vec![column],
                ref_schema: String::new(),
                ref_table,
                ref_columns: vec![ref_column],
                on_delete,
                on_update,
            });
        }
    }
}

/// 结构页只需要“约多少行”和“有没有行”：大表上的 COUNT(*) 会扫全表，
/// 占着连接几十秒甚至几分钟。这里用统计信息估算行数，再用 LIMIT 1 确认是否为空。
async fn count_nulls_mysql_count(conn: &mut mysql_async::Conn, database: &str, table: &str) -> Result<i64, String> {
    let qualified = qualified_table(Engine::Mysql, database, "", table)?;
    let probe = format!("SELECT 1 FROM {qualified} LIMIT 1");
    let any: Option<mysql_async::Row> = mysql_async::prelude::Queryable::query_first(conn, probe).await.map_err(|err| crate::error_text::chain(&err))?;
    if any.is_none() {
        return Ok(0);
    }
    let estimate: Option<mysql_async::Row> = mysql_async::prelude::Queryable::exec_first(
        conn,
        "SELECT TABLE_ROWS FROM information_schema.tables WHERE table_schema = ? AND table_name = ?",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(|err| crate::error_text::chain(&err))?;
    Ok(estimate.map(|row| mysql_i64(&row, 0)).unwrap_or(0).max(1))
}

pub(super) fn mysql_string(row: &mysql_async::Row, index: usize) -> String {
    match row.as_ref(index).unwrap_or(&mysql_async::Value::NULL) {
        mysql_async::Value::NULL => String::new(),
        mysql_async::Value::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        mysql_async::Value::Int(number) => number.to_string(),
        mysql_async::Value::UInt(number) => number.to_string(),
        mysql_async::Value::Float(number) => number.to_string(),
        mysql_async::Value::Double(number) => number.to_string(),
        mysql_async::Value::Date(year, month, day, hour, minute, second, micros) => {
            format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{micros:06}")
        }
        mysql_async::Value::Time(negative, days, hour, minute, second, micros) => {
            format!("{}{days} {hour:02}:{minute:02}:{second:02}.{micros:06}", if *negative { "-" } else { "" })
        }
    }
}

fn mysql_i64(row: &mysql_async::Row, index: usize) -> i64 {
    mysql_string(row, index).parse().unwrap_or(0)
}

pub(in crate::adapters) async fn run_steps(pool: &mysql_async::Pool, steps: &[Step]) -> Result<Vec<StepReceipt>, String> {
    let mut conn = pool.get_conn().await.map_err(|err| crate::error_text::chain(&err))?;
    let mut results = Vec::new();
    let mut stop = false;
    for step in steps {
        if stop {
            results.push(receipt(step, StepStatus::Skipped, "前面的语句没有成功，这条没有执行。"));
            continue;
        }
        match mysql_async::prelude::Queryable::query_drop(&mut conn, step.sql.clone()).await {
            Ok(()) => results.push(receipt(step, StepStatus::Ok, "")),
            Err(mysql_async::Error::Server(err)) => {
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

#[cfg(test)]
mod tests {
    use super::{mysql_index_kind, mysql_visibility, StatisticsVisibility};
    use crate::ddl::model::IndexKind;

    #[test]
    fn mysql_statistics_values_map_to_kind_and_visibility() {
        assert_eq!(mysql_index_kind("PRIMARY", "0", "BTREE"), IndexKind::Primary);
        assert_eq!(mysql_index_kind("uq", "0", "BTREE"), IndexKind::Unique);
        assert_eq!(mysql_index_kind("idx", "1", "HASH"), IndexKind::Normal);
        assert_eq!(mysql_index_kind("ft", "1", "FULLTEXT"), IndexKind::Fulltext);
        assert_eq!(mysql_index_kind("sp", "1", "SPATIAL"), IndexKind::Spatial);
        assert_eq!(mysql_visibility("YES"), Some(true));
        assert_eq!(mysql_visibility("NO"), Some(false));
        assert_eq!(mysql_visibility(""), None);
        assert_eq!(StatisticsVisibility::of_columns(&["IS_VISIBLE"]), StatisticsVisibility::IsVisible);
        assert_eq!(StatisticsVisibility::of_columns(&["IGNORED"]), StatisticsVisibility::Ignored);
        assert_eq!(StatisticsVisibility::of_columns::<&str>(&[]), StatisticsVisibility::Missing);
        assert!(StatisticsVisibility::Ignored.select_expr().contains("IGNORED"));
        assert_eq!(StatisticsVisibility::Missing.select_expr(), "NULL");
    }
}
