//! SQLite 的结构内省与执行：sqlite_master 与 PRAGMA 读表结构，按官方步骤在同一条连接上逐条执行重建。

use std::collections::BTreeMap;

use sqlx::{Row, ValueRef};

use crate::adapters::ddl::paren_list;
use crate::adapters::quote_ident;
use crate::connections::model::Engine;
use crate::ddl::exec::receipt;
use crate::ddl::introspect::keep_rows;
use crate::ddl::model::{
    Action, AttachedDatabase, Column, DependentSql, ForeignKey, IncomingForeignKey, Index, IndexColumn, IndexKind, Snapshot, Step, StepReceipt, StepStatus,
    SQLITE_REBUILD_RELEASE_SQL, SQLITE_REBUILD_SAVEPOINT, SQLITE_REBUILD_SAVEPOINT_SQL,
};
use crate::ddl::sql::{mentions, valid_name};

/// 改名前查新名字：SQLite 的表、视图、索引、触发器共用名字且不区分大小写。
pub(super) async fn name_taken(pool: &sqlx::SqlitePool, current: &str, name: &str) -> Result<bool, String> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE lower(name) = lower(?1) AND name <> ?2")
        .bind(name)
        .bind(current)
        .fetch_one(pool)
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    Ok(count > 0)
}

pub(super) async fn count(pool: &sqlx::SqlitePool, sql: &str) -> Result<i64, String> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned())).fetch_one(pool).await.map_err(|err| crate::error_text::chain(&err))
}

/// 会话状态：库版本与连接上当前的外键开关。
pub(super) async fn read_session_state(pool: &sqlx::SqlitePool, snapshot: &mut Snapshot) -> Result<(), String> {
    snapshot.sqlite_version = sqlx::query_scalar("SELECT sqlite_version()").fetch_one(pool).await.map_err(|err| crate::error_text::chain(&err))?;
    let live: i64 = sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(pool).await.map_err(|err| crate::error_text::chain(&err))?;
    snapshot.live_foreign_keys = Some(live != 0);
    Ok(())
}

pub(super) struct SqliteState {
    pub foreign_keys: Option<bool>,
    pub pragmas: BTreeMap<String, String>,
    pub attached: Vec<AttachedDatabase>,
}

pub(super) async fn sqlite_state(pool: &sqlx::SqlitePool) -> Result<SqliteState, String> {
    let mut pragmas = BTreeMap::new();
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(pool).await.map_err(|err| crate::error_text::chain(&err))?;
    pragmas.insert("foreign_keys".to_owned(), if foreign_keys != 0 { "ON" } else { "OFF" }.to_owned());
    let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode").fetch_one(pool).await.map_err(|err| crate::error_text::chain(&err))?;
    pragmas.insert("journal_mode".to_owned(), journal_mode);
    let user_version: i64 = sqlx::query_scalar("PRAGMA user_version").fetch_one(pool).await.map_err(|err| crate::error_text::chain(&err))?;
    pragmas.insert("user_version".to_owned(), user_version.to_string());
    let rows = sqlx::query("PRAGMA database_list").fetch_all(pool).await.map_err(|err| crate::error_text::chain(&err))?;
    let attached = rows
        .iter()
        .map(|row| AttachedDatabase { name: sqlite_text(row, 1).unwrap_or_default(), path: sqlite_text(row, 2).unwrap_or_default() })
        .collect::<Vec<_>>();
    Ok(SqliteState { foreign_keys: Some(foreign_keys != 0), pragmas, attached })
}

/// 维护动作完成后的结果反馈。读不到就只给一句“已完成”，不编造数字。
pub(super) async fn maintenance_note(pool: &sqlx::SqlitePool, action: &Action) -> Option<String> {
    if !matches!(action, Action::Vacuum | Action::Reindex { .. }) {
        return None;
    }
    match action {
        Action::Vacuum => {
            let pages: Option<i64> = sqlx::query_scalar("PRAGMA page_count").fetch_one(pool).await.ok();
            let free: Option<i64> = sqlx::query_scalar("PRAGMA freelist_count").fetch_one(pool).await.ok();
            Some(match (pages, free) {
                (Some(pages), Some(free)) => format!("VACUUM 已完成：数据库现在有 {pages} 页，其中空闲 {free} 页。"),
                _ => "VACUUM 已完成。".to_owned(),
            })
        }
        Action::Reindex { .. } => Some("REINDEX 已完成：索引已按当前的表结构重建。".to_owned()),
        _ => None,
    }
}

pub(super) async fn load_sqlite(pool: &sqlx::SqlitePool, database: &str, table: &str, recorded: Option<bool>) -> Result<Snapshot, String> {
    if !database.is_empty() && database != "main" {
        return Err(format!("当前连接打开的是 main，没有另开连接修改 {database}"));
    }
    if !valid_name(table) {
        return Err("表名无效。".to_owned());
    }
    let kind: Option<String> = sqlx::query_scalar("SELECT type FROM sqlite_master WHERE name = ?1 AND type IN ('table', 'view')")
        .bind(table)
        .fetch_optional(pool)
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    let Some(kind) = kind else {
        return Err(format!("找不到表或视图 {table}"));
    };
    let mut snapshot = Snapshot::bare(Engine::Sqlite, "main", "", table);
    snapshot.kind = if kind.eq_ignore_ascii_case("view") { "view" } else { "table" }.to_owned();
    snapshot.recorded_foreign_keys = recorded;
    snapshot.sqlite_version = sqlx::query_scalar("SELECT sqlite_version()").fetch_one(pool).await.map_err(|err| crate::error_text::chain(&err))?;
    let live: i64 = sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(pool).await.map_err(|err| crate::error_text::chain(&err))?;
    snapshot.live_foreign_keys = Some(live != 0);
    let table_sql: Option<String> = sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name = ?1 AND type IN ('table', 'view')")
        .bind(table)
        .fetch_optional(pool)
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    snapshot.table_sql = table_sql.unwrap_or_default();
    let upper = snapshot.table_sql.to_ascii_uppercase();
    snapshot.has_check = upper.contains("CHECK(") || upper.contains("CHECK (");
    snapshot.has_collate = upper.contains(" COLLATE ");
    snapshot.without_rowid = upper.contains("WITHOUT ROWID");
    snapshot.strict = upper.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_').any(|word| word == "STRICT");
    snapshot.autoincrement = upper.contains("AUTOINCREMENT");
    let quoted = quote_ident(Engine::Sqlite, table);
    let info = sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA table_xinfo({quoted})")))
        .fetch_all(pool)
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    for row in &info {
        let name = sqlite_text(row, 1)?;
        let hidden = sqlite_i64(row, 6).unwrap_or(0);
        snapshot.columns.push(Column {
            name,
            type_name: sqlite_text(row, 2).unwrap_or_default(),
            nullable: sqlite_i64(row, 3).unwrap_or(0) == 0,
            default_sql: sqlite_optional(row, 4),
            primary: sqlite_i64(row, 5).unwrap_or(0) > 0,
            auto: false,
            generated: hidden != 0,
            default_constraint: None,
        });
    }
    if snapshot.columns.is_empty() {
        return Err(format!("找不到表或视图 {table}"));
    }
    let primary_count = snapshot.columns.iter().filter(|item| item.primary).count();
    for column in &mut snapshot.columns {
        let integer_rowid = primary_count == 1 && column.primary && column.type_name.to_ascii_lowercase().contains("int");
        if column.primary && (!column.nullable || integer_rowid || snapshot.without_rowid || snapshot.strict) {
            column.nullable = false;
        }
    }
    let auto_name = if snapshot.autoincrement {
        let primaries: Vec<String> = snapshot.columns.iter().filter(|item| item.primary).map(|item| item.name.clone()).collect();
        if primaries.len() == 1 && snapshot.columns.iter().any(|item| item.name == primaries[0] && item.type_name.to_ascii_lowercase().contains("int")) {
            Some(primaries[0].clone())
        } else {
            None
        }
    } else {
        None
    };
    for column in &mut snapshot.columns {
        column.auto = auto_name.as_deref() == Some(column.name.as_str());
    }
    let indexes = keep_rows(
        &mut snapshot.index_error,
        sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA index_list({quoted})")))
            .fetch_all(pool)
            .await
            .map_err(|err| crate::error_text::chain(&err)),
    );
    for index in &indexes {
        if snapshot.index_error.is_some() {
            snapshot.indexes.clear();
            break;
        }
        let name = match sqlite_text(index, 1) {
            Ok(name) => name,
            Err(message) => {
                snapshot.indexes.clear();
                if snapshot.index_error.is_none() {
                    snapshot.index_error = Some(message);
                }
                break;
            }
        };
        let unique = sqlite_i64(index, 2).unwrap_or(0) != 0;
        let origin = sqlite_text(index, 3).unwrap_or_default();
        let partial = sqlite_i64(index, 4).unwrap_or(0) != 0;
        let info = match sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA index_xinfo({})", quote_ident(Engine::Sqlite, &name))))
            .fetch_all(pool)
            .await
        {
            Ok(rows) => rows,
            Err(err) => {
                snapshot.indexes.clear();
                if snapshot.index_error.is_none() {
                    snapshot.index_error = Some(crate::error_text::chain(&err));
                }
                break;
            }
        };
        let mut columns = Vec::new();
        let mut expression = false;
        for column in &info {
            match sqlite_optional(column, 2) {
                Some(name) if !name.is_empty() => columns.push(IndexColumn { descending: sqlite_i64(column, 3).unwrap_or(0) != 0, name, prefix: None }),
                _ => expression = true,
            }
        }
        let protected = origin == "pk" || origin == "u" || name.starts_with("sqlite_autoindex");
        let reason = if origin == "pk" || name.starts_with("sqlite_autoindex") {
            "由主键约束维护".to_owned()
        } else if origin == "u" {
            "由唯一约束维护".to_owned()
        } else {
            String::new()
        };
        let sql: Option<String> = match sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1")
            .bind(&name)
            .fetch_optional(pool)
            .await
        {
            Ok(sql) => sql,
            Err(err) => {
                snapshot.indexes.clear();
                if snapshot.index_error.is_none() {
                    snapshot.index_error = Some(crate::error_text::chain(&err));
                }
                break;
            }
        };
        let kind = IndexKind::infer(reason == "由主键约束维护", unique);
        snapshot.indexes.push(Index { name, unique, columns, protected, reason, partial, expression, sql, kind, ..Index::default() });
    }
    let fk_rows = keep_rows(
        &mut snapshot.foreign_key_error,
        sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA foreign_key_list({quoted})")))
            .fetch_all(pool)
            .await
            .map_err(|err| crate::error_text::chain(&err)),
    );
    if snapshot.foreign_key_error.is_none() {
        match group_sqlite_fks(table, &snapshot.table_sql, &fk_rows) {
            Ok(keys) => snapshot.foreign_keys = keys,
            Err(message) => snapshot.foreign_key_error = Some(message),
        }
    }
    match incoming_sqlite(pool, table).await {
        Ok(keys) => snapshot.incoming = keys,
        Err(message) => {
            if snapshot.foreign_key_error.is_none() {
                snapshot.foreign_key_error = Some(message);
            }
        }
    }
    let triggers = sqlx::query("SELECT name, COALESCE(sql, '') FROM sqlite_master WHERE type = 'trigger' AND tbl_name = ?1")
        .bind(table)
        .fetch_all(pool)
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    for row in &triggers {
        snapshot.triggers.push(DependentSql { name: sqlite_text(row, 0)?, sql: sqlite_text(row, 1).unwrap_or_default() });
    }
    let views = sqlx::query("SELECT name, COALESCE(sql, '') FROM sqlite_master WHERE type = 'view'")
        .fetch_all(pool)
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    for row in &views {
        let name = sqlite_text(row, 0)?;
        let sql = sqlite_text(row, 1).unwrap_or_default();
        if mentions(&sql, table) {
            snapshot.views.push(DependentSql { name, sql });
        }
    }
    snapshot.row_count = Some(count_rows_sqlite(pool, table).await?);
    snapshot.object_exists = true;
    if snapshot.kind == "view" && snapshot.table_sql.trim().is_empty() {
        snapshot.definition_error = Some("没有读到视图定义".to_owned());
    }
    Ok(snapshot)
}

async fn incoming_sqlite(pool: &sqlx::SqlitePool, table: &str) -> Result<Vec<IncomingForeignKey>, String> {
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'")
        .fetch_all(pool)
        .await
        .map_err(|err| crate::error_text::chain(&err))?;
    let mut incoming = Vec::new();
    for name in names {
        if name == table {
            continue;
        }
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA foreign_key_list({})", quote_ident(Engine::Sqlite, &name))))
            .fetch_all(pool)
            .await
            .map_err(|err| crate::error_text::chain(&err))?;
        for key in group_sqlite_fks(&name, "", &rows)? {
            if key.ref_table == table {
                incoming.push(IncomingForeignKey {
                    name: format!("{}.{}", name, key.name),
                    table: name.clone(),
                    schema: String::new(),
                    columns: key.columns,
                    ref_columns: key.ref_columns,
                });
            }
        }
    }
    Ok(incoming)
}

fn group_sqlite_fks(table: &str, create_sql: &str, rows: &[sqlx::sqlite::SqliteRow]) -> Result<Vec<ForeignKey>, String> {
    let parsed = constraint_names(create_sql);
    let mut groups: Vec<(i64, ForeignKey)> = Vec::new();
    for row in rows {
        let id = sqlite_i64(row, 0).unwrap_or(0);
        let seq = sqlite_i64(row, 1).unwrap_or(0) as usize;
        let ref_table = sqlite_text(row, 2)?;
        let from = sqlite_text(row, 3)?;
        let to = sqlite_text(row, 4)?;
        let on_update = sqlite_text(row, 5).unwrap_or_else(|_| "NO ACTION".to_owned());
        let on_delete = sqlite_text(row, 6).unwrap_or_else(|_| "NO ACTION".to_owned());
        if let Some(group) = groups.iter_mut().find(|group| group.0 == id) {
            let key = &mut group.1;
            if key.columns.len() <= seq {
                key.columns.resize(seq + 1, String::new());
                key.ref_columns.resize(seq + 1, String::new());
            }
            key.columns[seq] = from;
            key.ref_columns[seq] = to;
        } else {
            let mut columns = vec![String::new(); seq + 1];
            let mut ref_columns = vec![String::new(); seq + 1];
            columns[seq] = from;
            ref_columns[seq] = to;
            groups.push((
                id,
                ForeignKey {
                    name: format!("fk_{table}_{id}"),
                    columns,
                    ref_schema: String::new(),
                    ref_table,
                    ref_columns,
                    on_delete,
                    on_update,
                },
            ));
        }
    }
    for key in groups.iter_mut().map(|group| &mut group.1) {
        if let Some((name, _)) = parsed.iter().find(|(_, columns)| columns == &key.columns) {
            if !name.is_empty() {
                key.name = name.clone();
            }
        }
    }
    Ok(groups.into_iter().map(|group| group.1).collect())
}

fn constraint_names(sql: &str) -> Vec<(String, Vec<String>)> {
    let lower = sql.to_ascii_lowercase();
    let mut found = Vec::new();
    let mut offset = 0;
    while let Some(pos) = lower[offset..].find("foreign key") {
        let absolute = offset + pos;
        let name = constraint_before(&sql[..absolute]).unwrap_or_default();
        let columns = paren_list(&sql[absolute + "foreign key".len()..]).unwrap_or_default();
        found.push((name, columns));
        offset = absolute + "foreign key".len();
    }
    found
}

fn constraint_before(before: &str) -> Option<String> {
    let lower = before.to_ascii_lowercase();
    let pos = lower.rfind("constraint")?;
    let rest = before[pos + "constraint".len()..].trim();
    let mut chars = rest.chars().peekable();
    if chars.peek() == Some(&'"') {
        chars.next();
        let mut name = String::new();
        while let Some(ch) = chars.next() {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    name.push('"');
                } else {
                    break;
                }
            } else {
                name.push(ch);
            }
        }
        return Some(name);
    }
    Some(chars.take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_').collect())
}

async fn count_rows_sqlite(pool: &sqlx::SqlitePool, table: &str) -> Result<i64, String> {
    let sql = format!("SELECT COUNT(*) FROM {}", quote_ident(Engine::Sqlite, table));
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql)).fetch_one(pool).await.map_err(|err| crate::error_text::chain(&err))
}

fn sqlite_text(row: &sqlx::sqlite::SqliteRow, index: usize) -> Result<String, String> {
    if let Ok(value) = row.try_get::<Option<String>, _>(index) {
        return value.ok_or_else(|| "元数据是空的".to_owned());
    }
    if let Ok(value) = row.try_get::<Option<i64>, _>(index) {
        return value.map(|number| number.to_string()).ok_or_else(|| "元数据是空的".to_owned());
    }
    Err(format!("读不到第 {index} 列"))
}

fn sqlite_optional(row: &sqlx::sqlite::SqliteRow, index: usize) -> Option<String> {
    sqlite_text(row, index).ok()
}

fn sqlite_i64(row: &sqlx::sqlite::SqliteRow, index: usize) -> Option<i64> {
    if let Ok(value) = row.try_get::<Option<i64>, _>(index) {
        return value;
    }
    if let Ok(value) = row.try_get::<Option<String>, _>(index) {
        return value.and_then(|text| text.parse().ok());
    }
    None
}

pub(in crate::adapters) async fn run_steps(pool: &sqlx::SqlitePool, steps: &[Step], restore: Option<bool>) -> Result<Vec<StepReceipt>, String> {
    let mut conn = pool.acquire().await.map_err(|err| crate::error_text::chain(&err))?;
    let mut results = Vec::new();
    let mut stop = false;
    let mut unknown = false;
    // 失败补偿的边界：内部重建事务，或活动外层事务里的重建保存点（ADR-4）。
    let mut boundary: Option<Boundary> = None;
    for step in steps {
        if stop {
            results.push(receipt(step, StepStatus::Skipped, "前面的语句没有成功，这条没有执行。"));
            continue;
        }
        let kind = statement_kind(&step.sql);
        let executed: Result<Option<String>, String> = match kind {
            StmtKind::Check => sqlx::query(sqlx::AssertSqlSafe(step.sql.clone()))
                .fetch_all(&mut *conn)
                .await
                .map(|rows| {
                    if rows.is_empty() {
                        Ok(None)
                    } else {
                        Err(format!("foreign_key_check 发现 {} 处违规，不会把这次重建当成成功。", rows.len()))
                    }
                })
                .map_err(|err| crate::error_text::chain(&err))
                .and_then(|item| item),
            // PRAGMA 可能带回一行（journal_mode 就会），用 fetch_all 接住并当成结果反馈。
            StmtKind::Probe => sqlx::query(sqlx::AssertSqlSafe(step.sql.clone()))
                .fetch_all(&mut *conn)
                .await
                .map(|rows| Ok::<Option<String>, String>(probe_note(&rows)))
                .map_err(|err| crate::error_text::chain(&err))
                .and_then(|item| item),
            _ => sqlx::query(sqlx::AssertSqlSafe(step.sql.clone()))
                .execute(&mut *conn)
                .await
                .map(|_| Ok::<Option<String>, String>(None))
                .map_err(|err| crate::error_text::chain(&err))
                .and_then(|item| item),
        };
        match executed {
            Ok(note) => {
                // 记下边界步骤在 results 里的位置（本条回执还没有推进）。
                if kind == StmtKind::Begin {
                    boundary = Some(Boundary::Transaction { step: results.len() });
                } else if kind == StmtKind::Savepoint {
                    boundary = Some(Boundary::Savepoint { step: results.len() });
                } else if kind == StmtKind::Commit || kind == StmtKind::Release {
                    boundary = None;
                }
                results.push(receipt(step, StepStatus::Ok, note.as_deref().unwrap_or("")));
            }
            Err(message) if sqlite_unknown(&message) => {
                unknown = true;
                stop = true;
                results.push(receipt(step, StepStatus::Unknown, &message));
            }
            Err(message) => {
                stop = true;
                results.push(receipt(step, StepStatus::Failed, &message));
                match boundary.take() {
                    Some(Boundary::Transaction { step: start }) => match sqlx::query(sqlx::AssertSqlSafe("ROLLBACK".to_owned())).execute(&mut *conn).await {
                        Ok(_) => {
                            for item in results.iter_mut().skip(start) {
                                if item.status == StepStatus::Ok {
                                    item.status = StepStatus::RolledBack;
                                    item.message = "重建事务已回滚，这条语句没有保留。".to_owned();
                                }
                            }
                        }
                        Err(err) => {
                            unknown = true;
                            for item in results.iter_mut().skip(start) {
                                if item.status == StepStatus::Ok {
                                    item.status = StepStatus::Unknown;
                                    item.message = format!("回滚重建事务失败，结果未知：{err}");
                                }
                            }
                        }
                    },
                    Some(Boundary::Savepoint { step: start }) => {
                        unknown = !rollback_rebuild_savepoint(&mut conn, &mut results, start).await;
                    }
                    None => {}
                }
            }
        }
    }
    if !unknown {
        restore_foreign_keys(&mut conn, &mut results, restore).await;
    }
    Ok(results)
}

/// 重建失败时的补偿边界。
#[derive(Clone, Copy)]
enum Boundary {
    /// 没有外层事务：原有内部重建事务，可整体 `ROLLBACK`。
    Transaction { step: usize },
    /// 活动外层事务里的保存点：只 `ROLLBACK TO` 并释放，不撤销此前已经应用的修改。
    Savepoint { step: usize },
}

/// 只回滚到重建保存点并释放它。返回 false 表示补偿本身失败（结果未知）。
async fn rollback_rebuild_savepoint(conn: &mut sqlx::SqliteConnection, results: &mut [StepReceipt], start: usize) -> bool {
    let rollback = format!("ROLLBACK TO SAVEPOINT {SQLITE_REBUILD_SAVEPOINT};");
    let outcome = async {
        sqlx::query(sqlx::AssertSqlSafe(rollback)).execute(&mut *conn).await.map_err(|err| crate::error_text::chain(&err))?;
        sqlx::query(sqlx::AssertSqlSafe(SQLITE_REBUILD_RELEASE_SQL.to_owned()))
            .execute(&mut *conn)
            .await
            .map(|_| ())
            .map_err(|err| crate::error_text::chain(&err))
    }
    .await;
    match outcome {
        Ok(()) => {
            // 保存点本身保留 Ok：它确实建立过。之后的步骤都没有保留。
            for item in results.iter_mut().skip(start + 1) {
                if item.status == StepStatus::Ok {
                    item.status = StepStatus::RolledBack;
                    item.message = "已回滚到重建保存点：这条语句没有保留，此前已经应用的修改仍在当前事务里。".to_owned();
                }
            }
            true
        }
        Err(err) => {
            for item in results.iter_mut().skip(start + 1) {
                if item.status == StepStatus::Ok {
                    item.status = StepStatus::Unknown;
                    item.message = format!("回滚到重建保存点失败，结果未知：{err}");
                }
            }
            false
        }
    }
}

async fn restore_foreign_keys(conn: &mut sqlx::SqliteConnection, results: &mut Vec<StepReceipt>, restore: Option<bool>) {
    let turned_off = results.iter().any(|step| step.status == StepStatus::Ok && is_fk_off(&step.sql));
    if !turned_off {
        return;
    }
    let value = if restore.unwrap_or(false) { "ON" } else { "OFF" };
    let sql = format!("PRAGMA foreign_keys = {value};");
    if results.iter().any(|step| step.status == StepStatus::Ok && step.sql.trim().eq_ignore_ascii_case(sql.trim())) {
        return;
    }
    let outcome = sqlx::query(sqlx::AssertSqlSafe(sql.clone())).execute(&mut *conn).await;
    if let Some(step) = results.iter_mut().rev().find(|step| step.sql.trim().eq_ignore_ascii_case(sql.trim())) {
        match outcome {
            Ok(_) => {
                step.status = StepStatus::Ok;
                step.message = "已恢复会话原来的 foreign_keys。".to_owned();
            }
            Err(err) => {
                step.status = StepStatus::Unknown;
                step.message = format!("恢复 foreign_keys 失败，结果未知：{err}");
            }
        }
        return;
    }
    results.push(StepReceipt {
        sql,
        label: "恢复会话原来的 foreign_keys".to_owned(),
        status: if outcome.is_ok() { StepStatus::Ok } else { StepStatus::Unknown },
        message: match &outcome {
            Ok(_) => "已恢复会话原来的 foreign_keys。".to_owned(),
            Err(err) => format!("恢复 foreign_keys 失败，结果未知：{err}"),
        },
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StmtKind {
    Begin,
    Commit,
    /// 活动外层事务里的重建保存点边界（ADR-4），由执行层生成。
    Savepoint,
    /// 重建保存点的释放边界。
    Release,
    Check,
    /// 会带回结果的 PRAGMA：execute 会因为“返回了结果”报错，必须用 fetch_all。
    Probe,
    Other,
}

fn statement_kind(sql: &str) -> StmtKind {
    let text = sql.trim().trim_end_matches(';').trim().to_ascii_lowercase();
    if text == "begin" || text.starts_with("begin ") {
        StmtKind::Begin
    } else if text == "commit" {
        StmtKind::Commit
    } else if text.eq_ignore_ascii_case(SQLITE_REBUILD_SAVEPOINT_SQL.trim_end_matches(';')) {
        StmtKind::Savepoint
    } else if text.eq_ignore_ascii_case(SQLITE_REBUILD_RELEASE_SQL.trim_end_matches(';')) {
        StmtKind::Release
    } else if text.starts_with("pragma foreign_key_check") {
        StmtKind::Check
    } else if text.starts_with("pragma ") {
        StmtKind::Probe
    } else {
        StmtKind::Other
    }
}

/// PRAGMA 的回显就是结果反馈（例如 journal_mode 会回显新模式）。
fn probe_note(rows: &[sqlx::sqlite::SqliteRow]) -> Option<String> {
    let row = rows.first()?;
    let raw = row.try_get_raw(0).ok()?;
    if raw.is_null() {
        return None;
    }
    if let Ok(text) = row.try_get::<String, _>(0) {
        return Some(format!("数据库返回：{text}"));
    }
    if let Ok(number) = row.try_get::<i64, _>(0) {
        return Some(format!("数据库返回：{number}"));
    }
    if let Ok(number) = row.try_get::<f64, _>(0) {
        return Some(format!("数据库返回：{number}"));
    }
    None
}

fn is_fk_off(sql: &str) -> bool {
    pragma_foreign_keys_is(sql, false)
}

pub(super) fn pragma_foreign_keys_is(sql: &str, enabled: bool) -> bool {
    let text = sql.trim().trim_end_matches(';').trim().to_ascii_lowercase().replace(' ', "");
    let want = if enabled { "pragmaforeign_keys=on" } else { "pragmaforeign_keys=off" };
    text == want
}

fn sqlite_unknown(message: &str) -> bool {
    let text = message.to_ascii_lowercase();
    text.contains("pool closed") || text.contains("connection closed") || text.contains("broken pipe")
}
