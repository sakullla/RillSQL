use sqlx::Row;

use crate::connections::CatalogSession;

use super::model::{Action, Column, DependentSql, Engine, ForeignKey, IncomingForeignKey, Index, IndexColumn, Request, Snapshot};
use super::sql::{mentions, qualified_table, quote_ident};

pub async fn load(session: &CatalogSession, request: &Request, table: &str) -> Result<Snapshot, String> {
    ensure_session(session, request.engine)?;
    let recorded = match session {
        CatalogSession::Sqlite { foreign_keys, .. } => *foreign_keys,
        _ => None,
    };
    match session {
        CatalogSession::Scripted => Err("这个连接没有连着可查询的数据库。".to_owned()),
        CatalogSession::Sqlite { pool, .. } => load_sqlite(pool, &request.database, table, recorded).await,
        CatalogSession::Mysql(pool) => load_mysql(pool, request.engine, &request.database, table).await,
        CatalogSession::Postgres(client) => load_postgres(client, &request.database, &request.schema, table).await,
    }
}

pub async fn enrich(session: &CatalogSession, snapshot: &mut Snapshot, action: &Action) -> Result<(), String> {
    let Action::AlterColumns { columns, .. } = action else {
        return Ok(());
    };
    for draft in columns {
        let Some(original) = draft.original.as_deref() else {
            continue;
        };
        let Some(column) = snapshot.columns.iter().find(|column| column.name == original) else {
            continue;
        };
        if !draft.dropped && !draft.nullable && column.nullable {
            let count = count_nulls(session, snapshot, original).await?;
            snapshot.null_counts.insert(original.to_owned(), count);
        }
    }
    Ok(())
}

pub fn table_of(action: &Action) -> Option<&str> {
    match action {
        Action::CreateTable { .. } | Action::SetForeignKeys { .. } => None,
        Action::AlterColumns { table, .. }
        | Action::CreateIndex { table, .. }
        | Action::DropIndex { table, .. }
        | Action::CreateForeignKey { table, .. }
        | Action::DropForeignKey { table, .. }
        | Action::DropTable { table }
        | Action::ReplaceView { table, .. } => Some(table.as_str()),
    }
}

pub async fn attach_reference(session: &CatalogSession, request: &Request, snapshot: &mut Snapshot, action: &Action) -> Result<(), String> {
    let Action::CreateForeignKey { foreign_key, .. } = action else {
        return Ok(());
    };
    let mut target_request = request.clone();
    if request.engine == Engine::Postgres {
        target_request.schema = foreign_key.ref_schema.clone();
    }
    let referenced = load(session, &target_request, &foreign_key.ref_table).await?;
    if referenced.kind != "table" {
        return Err(format!("目标 {} 不是基础表。", foreign_key.ref_table));
    }
    snapshot.reference = Some(Box::new(referenced));
    Ok(())
}

pub async fn session_snapshot(session: &CatalogSession, request: &Request) -> Result<Snapshot, String> {
    ensure_session(session, request.engine)?;
    if request.engine != Engine::Sqlite {
        return Err("只有 SQLite 用连接上的 foreign_keys 控制外键是否生效。".to_owned());
    }
    let CatalogSession::Sqlite { pool, foreign_keys } = session else {
        return Err("会话类型和目标数据库不一致，没有另开连接。".to_owned());
    };
    let mut snapshot = Snapshot::bare(Engine::Sqlite, &request.database, &request.schema, "");
    snapshot.kind = "session".to_owned();
    snapshot.object_exists = false;
    snapshot.table_sql.clear();
    snapshot.recorded_foreign_keys = *foreign_keys;
    snapshot.sqlite_version = sqlx::query_scalar("SELECT sqlite_version()").fetch_one(pool).await.map_err(|err| err.to_string())?;
    let live: i64 = sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(pool).await.map_err(|err| err.to_string())?;
    snapshot.live_foreign_keys = Some(live != 0);
    Ok(snapshot)
}

fn ensure_session(session: &CatalogSession, engine: Engine) -> Result<(), String> {
    let ok = match (session, engine) {
        (CatalogSession::Sqlite { .. }, Engine::Sqlite) => true,
        (CatalogSession::Mysql(_), Engine::Mysql | Engine::Mariadb) => true,
        (CatalogSession::Postgres(_), Engine::Postgres) => true,
        (CatalogSession::Scripted, _) => return Err("这个连接没有连着可查询的数据库。".to_owned()),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err("会话类型和目标数据库不一致，没有另开连接。".to_owned())
    }
}

async fn load_sqlite(pool: &sqlx::SqlitePool, database: &str, table: &str, recorded: Option<bool>) -> Result<Snapshot, String> {
    if !database.is_empty() && database != "main" {
        return Err(format!("当前连接打开的是 main，没有另开连接修改 {database}"));
    }
    if !super::sql::valid_name(table) {
        return Err("表名无效。".to_owned());
    }
    let kind: Option<String> = sqlx::query_scalar("SELECT type FROM sqlite_master WHERE name = ?1 AND type IN ('table', 'view')")
        .bind(table)
        .fetch_optional(pool)
        .await
        .map_err(|err| err.to_string())?;
    let Some(kind) = kind else {
        return Err(format!("找不到表或视图 {table}"));
    };
    let mut snapshot = Snapshot::bare(Engine::Sqlite, "main", "", table);
    snapshot.kind = if kind.eq_ignore_ascii_case("view") { "view" } else { "table" }.to_owned();
    snapshot.recorded_foreign_keys = recorded;
    snapshot.sqlite_version = sqlx::query_scalar("SELECT sqlite_version()").fetch_one(pool).await.map_err(|err| err.to_string())?;
    let live: i64 = sqlx::query_scalar("PRAGMA foreign_keys").fetch_one(pool).await.map_err(|err| err.to_string())?;
    snapshot.live_foreign_keys = Some(live != 0);
    let table_sql: Option<String> = sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name = ?1 AND type IN ('table', 'view')")
        .bind(table)
        .fetch_optional(pool)
        .await
        .map_err(|err| err.to_string())?;
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
        .map_err(|err| err.to_string())?;
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
    let indexes = sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA index_list({quoted})")))
        .fetch_all(pool)
        .await
        .map_err(|err| err.to_string())?;
    for index in &indexes {
        let name = sqlite_text(index, 1)?;
        let unique = sqlite_i64(index, 2).unwrap_or(0) != 0;
        let origin = sqlite_text(index, 3).unwrap_or_default();
        let partial = sqlite_i64(index, 4).unwrap_or(0) != 0;
        let info = sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA index_xinfo({})", quote_ident(Engine::Sqlite, &name))))
            .fetch_all(pool)
            .await
            .map_err(|err| err.to_string())?;
        let mut columns = Vec::new();
        let mut expression = false;
        for column in &info {
            match sqlite_optional(column, 2) {
                Some(name) if !name.is_empty() => columns.push(IndexColumn { descending: sqlite_i64(column, 3).unwrap_or(0) != 0, name }),
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
        let sql: Option<String> = sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1")
            .bind(&name)
            .fetch_optional(pool)
            .await
            .map_err(|err| err.to_string())?;
        snapshot.indexes.push(Index { name, unique, columns, protected, reason, partial, expression, sql });
    }
    let fk_rows = sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA foreign_key_list({quoted})")))
        .fetch_all(pool)
        .await
        .map_err(|err| err.to_string())?;
    snapshot.foreign_keys = group_sqlite_fks(table, &snapshot.table_sql, &fk_rows)?;
    snapshot.incoming = incoming_sqlite(pool, table).await?;
    let triggers = sqlx::query("SELECT name, COALESCE(sql, '') FROM sqlite_master WHERE type = 'trigger' AND tbl_name = ?1")
        .bind(table)
        .fetch_all(pool)
        .await
        .map_err(|err| err.to_string())?;
    for row in &triggers {
        snapshot.triggers.push(DependentSql { name: sqlite_text(row, 0)?, sql: sqlite_text(row, 1).unwrap_or_default() });
    }
    let views = sqlx::query("SELECT name, COALESCE(sql, '') FROM sqlite_master WHERE type = 'view'")
        .fetch_all(pool)
        .await
        .map_err(|err| err.to_string())?;
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
        .map_err(|err| err.to_string())?;
    let mut incoming = Vec::new();
    for name in names {
        if name == table {
            continue;
        }
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA foreign_key_list({})", quote_ident(Engine::Sqlite, &name))))
            .fetch_all(pool)
            .await
            .map_err(|err| err.to_string())?;
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

fn paren_list(text: &str) -> Option<Vec<String>> {
    let start = text.find('(')?;
    let end = text[start + 1..].find(')')? + start + 1;
    Some(
        text[start + 1..end]
            .split(',')
            .map(|part| part.trim().trim_matches('"').trim_matches('`').to_owned())
            .filter(|part| !part.is_empty())
            .collect(),
    )
}

async fn count_rows_sqlite(pool: &sqlx::SqlitePool, table: &str) -> Result<i64, String> {
    let sql = format!("SELECT COUNT(*) FROM {}", quote_ident(Engine::Sqlite, table));
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql)).fetch_one(pool).await.map_err(|err| err.to_string())
}

async fn count_nulls(session: &CatalogSession, snapshot: &Snapshot, column: &str) -> Result<i64, String> {
    let qualified = qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, &snapshot.name)?;
    let sql = format!("SELECT COUNT(*) FROM {qualified} WHERE {} IS NULL", quote_ident(snapshot.engine, column));
    match session {
        CatalogSession::Sqlite { pool, .. } => sqlx::query_scalar(sqlx::AssertSqlSafe(sql)).fetch_one(pool).await.map_err(|err| err.to_string()),
        CatalogSession::Postgres(client) => {
            let row = client.query_one(&sql, &[]).await.map_err(|err| err.to_string())?;
            row.try_get(0).map_err(|err| err.to_string())
        }
        CatalogSession::Mysql(pool) => {
            let mut conn = pool.get_conn().await.map_err(|err| err.to_string())?;
            let row: Option<mysql_async::Row> = mysql_async::prelude::Queryable::query_first(&mut conn, sql).await.map_err(|err| err.to_string())?;
            let row = row.ok_or_else(|| "没有读到 NULL 计数".to_owned())?;
            Ok(mysql_i64(&row, 0))
        }
        CatalogSession::Scripted => Err("这个连接没有连着可查询的数据库。".to_owned()),
    }
}

async fn load_mysql(pool: &mysql_async::Pool, engine: Engine, database: &str, table: &str) -> Result<Snapshot, String> {
    if database.is_empty() {
        return Err("需要数据库名，没有另开连接。".to_owned());
    }
    let mut conn = pool.get_conn().await.map_err(|err| err.to_string())?;
    let kind: Option<mysql_async::Row> = mysql_async::prelude::Queryable::exec_first(
        &mut conn,
        "SELECT TABLE_TYPE, ENGINE FROM information_schema.tables WHERE table_schema = ? AND table_name = ?",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(|err| err.to_string())?;
    let Some(kind_row) = kind else {
        return Err(format!("找不到表或视图 {database}.{table}"));
    };
    let mut snapshot = Snapshot::bare(engine, database, database, table);
    snapshot.kind = if mysql_string(&kind_row, 0).eq_ignore_ascii_case("VIEW") { "view" } else { "table" }.to_owned();
    snapshot.storage = Some(mysql_string(&kind_row, 1));
    let columns: Vec<mysql_async::Row> = mysql_async::prelude::Queryable::exec(
        &mut conn,
        "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, EXTRA, GENERATION_EXPRESSION FROM information_schema.columns WHERE table_schema = ? AND table_name = ? ORDER BY ORDINAL_POSITION",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(|err| err.to_string())?;
    for row in &columns {
        let extra = mysql_string(row, 4).to_ascii_lowercase();
        let generated = !mysql_string(row, 5).is_empty() || extra.contains("generated");
        snapshot.columns.push(Column {
            name: mysql_string(row, 0),
            type_name: mysql_string(row, 1),
            nullable: mysql_string(row, 2).eq_ignore_ascii_case("YES"),
            default_sql: {
                let value = mysql_string(row, 3);
                if value.is_empty() { None } else { Some(value) }
            },
            primary: false,
            auto: extra.contains("auto_increment"),
            generated,
        });
    }
    let indexes: Vec<mysql_async::Row> = mysql_async::prelude::Queryable::exec(
        &mut conn,
        "SELECT INDEX_NAME, NON_UNIQUE, COLUMN_NAME, COLLATION FROM information_schema.statistics WHERE table_schema = ? AND table_name = ? ORDER BY INDEX_NAME, SEQ_IN_INDEX",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(|err| err.to_string())?;
    for row in &indexes {
        let name = mysql_string(row, 0);
        let column = IndexColumn { name: mysql_string(row, 2), descending: mysql_string(row, 3).eq_ignore_ascii_case("D") };
        if let Some(index) = snapshot.indexes.iter_mut().find(|index| index.name == name) {
            index.columns.push(column);
        } else {
            let primary = name.eq_ignore_ascii_case("PRIMARY");
            snapshot.indexes.push(Index {
                name,
                unique: mysql_string(row, 1) == "0",
                columns: vec![column],
                protected: primary,
                reason: if primary { "由主键约束维护".to_owned() } else { String::new() },
                partial: false,
                expression: false,
                sql: None,
            });
        }
    }
    for column in &mut snapshot.columns {
        column.primary = snapshot.indexes.iter().any(|index| index.name.eq_ignore_ascii_case("PRIMARY") && index.columns.iter().any(|item| item.name == column.name));
    }
    let foreign_keys: Vec<mysql_async::Row> = mysql_async::prelude::Queryable::exec(
        &mut conn,
        "SELECT k.CONSTRAINT_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_NAME, k.REFERENCED_COLUMN_NAME, r.DELETE_RULE, r.UPDATE_RULE FROM information_schema.KEY_COLUMN_USAGE k JOIN information_schema.REFERENTIAL_CONSTRAINTS r ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME AND r.TABLE_NAME = k.TABLE_NAME WHERE k.TABLE_SCHEMA = ? AND k.TABLE_NAME = ? AND k.REFERENCED_TABLE_NAME IS NOT NULL ORDER BY k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(|err| err.to_string())?;
    push_mysql_fks(&mut snapshot.foreign_keys, &foreign_keys, false);
    for key in &snapshot.foreign_keys {
        if let Some(index) = snapshot.indexes.iter_mut().find(|index| index.name == key.name) {
            index.protected = true;
            index.reason = format!("外键 {} 依赖此索引", key.name);
        }
    }
    let incoming: Vec<mysql_async::Row> = mysql_async::prelude::Queryable::exec(
        &mut conn,
        "SELECT k.CONSTRAINT_NAME, k.TABLE_NAME, k.COLUMN_NAME, k.REFERENCED_COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE k WHERE k.REFERENCED_TABLE_SCHEMA = ? AND k.REFERENCED_TABLE_NAME = ? AND k.REFERENCED_COLUMN_NAME IS NOT NULL ORDER BY k.CONSTRAINT_NAME, k.ORDINAL_POSITION",
        (database.to_owned(), table.to_owned()),
    )
    .await
    .map_err(|err| err.to_string())?;
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
        snapshot.table_sql = format!("-- {}.{table}", database);
    }
    Ok(snapshot)
}

async fn mysql_view_definition(conn: &mut mysql_async::Conn, database: &str, table: &str) -> Result<String, String> {
    let sql = format!(
        "SHOW CREATE VIEW {}.{}",
        quote_ident(Engine::Mysql, database),
        quote_ident(Engine::Mysql, table)
    );
    let row: Option<mysql_async::Row> = mysql_async::prelude::Queryable::query_first(conn, sql).await.map_err(|err| err.to_string())?;
    let row = row.ok_or_else(|| "没有读到视图定义".to_owned())?;
    let text = mysql_string(&row, 1);
    if text.trim().is_empty() || text.trim().starts_with("--") {
        return Err("没有读到视图定义".to_owned());
    }
    Ok(text)
}

fn push_mysql_fks(target: &mut Vec<ForeignKey>, rows: &[mysql_async::Row], _incoming: bool) {
    for row in rows {
        let name = mysql_string(row, 0);
        let column = mysql_string(row, 1);
        let ref_table = mysql_string(row, 2);
        let ref_column = mysql_string(row, 3);
        let on_delete = mysql_string(row, 4);
        let on_update = mysql_string(row, 5);
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

async fn count_nulls_mysql_count(conn: &mut mysql_async::Conn, database: &str, table: &str) -> Result<i64, String> {
    let sql = format!("SELECT COUNT(*) FROM {}", qualified_table(Engine::Mysql, database, "", table)?);
    let row: Option<mysql_async::Row> = mysql_async::prelude::Queryable::query_first(conn, sql).await.map_err(|err| err.to_string())?;
    Ok(mysql_i64(&row.ok_or_else(|| "没有读到行数".to_owned())?, 0))
}

async fn load_postgres(client: &tokio_postgres::Client, database: &str, schema: &str, table: &str) -> Result<Snapshot, String> {
    if schema.is_empty() {
        return Err("PostgreSQL 需要指定 schema（表所在的分组名），没有另开连接。".to_owned());
    }
    let current = client.query_one("SELECT current_database()", &[]).await.map_err(|err| err.to_string())?;
    let current: Option<String> = current.try_get(0).map_err(|err| err.to_string())?;
    if current.as_deref().unwrap_or("") != database {
        return Err(format!("当前连接打开的是 {}，没有另开连接修改 {database}", current.unwrap_or_default()));
    }
    let kind_rows = client
        .query(
            "SELECT c.relkind::text, c.oid FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('r','p','v','m')",
            &[&schema, &table],
        )
        .await
        .map_err(|err| err.to_string())?;
    let Some(kind_row) = kind_rows.first() else {
        return Err(format!("找不到表或视图 {schema}.{table}"));
    };
    let kind_text: String = kind_row.get(0);
    let oid: u32 = kind_row.get(1);
    let mut snapshot = Snapshot::bare(Engine::Postgres, database, schema, table);
    snapshot.kind = if kind_text == "r" || kind_text == "p" { "table" } else { "view" }.to_owned();
    snapshot.materialized = kind_text == "m";
    let columns = client
        .query(
            "SELECT a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod), a.attnotnull, pg_get_expr(ad.adbin, ad.adrelid), a.attidentity::text, a.attgenerated::text FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid LEFT JOIN pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum WHERE a.attrelid = $1 AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
            &[&oid],
        )
        .await
        .map_err(|err| err.to_string())?;
    for row in &columns {
        let identity: String = row.get(4);
        let generated: String = row.get(5);
        let default_sql: Option<String> = row.get(3);
        snapshot.columns.push(Column {
            name: row.get(0),
            type_name: row.get(1),
            nullable: !row.get::<_, bool>(2),
            default_sql,
            primary: false,
            auto: identity == "a" || identity == "d",
            generated: !generated.is_empty(),
        });
    }
    let indexes = client
        .query(
            "SELECT i.relname, ix.indisunique, ix.indisprimary, ix.indpred IS NOT NULL, ix.indexprs IS NOT NULL, pg_get_indexdef(ix.indexrelid), a.attname, array_position(ix.indkey, a.attnum), COALESCE((ix.indoption[array_position(ix.indkey, a.attnum) - 1] & 1) = 1, false) FROM pg_index ix JOIN pg_class i ON i.oid = ix.indexrelid LEFT JOIN pg_attribute a ON a.attrelid = ix.indrelid AND a.attnum = ANY (ix.indkey) WHERE ix.indrelid = $1 ORDER BY i.relname, array_position(ix.indkey, a.attnum)",
            &[&oid],
        )
        .await
        .map_err(|err| err.to_string())?;
    for row in &indexes {
        let name: String = row.get(0);
        let column_name: Option<String> = row.get(6);
        let descending: bool = row.get(8);
        let column = column_name.map(|name| IndexColumn { name, descending });
        if let Some(index) = snapshot.indexes.iter_mut().find(|index| index.name == name) {
            if let Some(column) = column {
                index.columns.push(column);
            } else {
                index.expression = true;
            }
        } else {
            let primary: bool = row.get(2);
            let expression = row.get::<_, bool>(4) || column.is_none();
            snapshot.indexes.push(Index {
                name,
                unique: row.get(1),
                columns: column.into_iter().collect(),
                protected: primary,
                reason: if primary { "由主键约束维护".to_owned() } else { String::new() },
                partial: row.get(3),
                expression,
                sql: row.get(5),
            });
        }
    }
    let constraints = client
        .query(
            "SELECT con.contype::text, con.conname, pg_get_constraintdef(con.oid), i.relname FROM pg_constraint con LEFT JOIN pg_class i ON i.oid = con.conindid WHERE con.conrelid = $1",
            &[&oid],
        )
        .await
        .map_err(|err| err.to_string())?;
    for row in &constraints {
        let contype: String = row.get(0);
        let name: String = row.get(1);
        let definition: String = row.get(2);
        let index_name: Option<String> = row.get(3);
        if contype == "c" {
            snapshot.has_check = true;
        }
        if matches!(contype.as_str(), "p" | "u" | "f") {
            if let Some(index_name) = index_name {
                if let Some(index) = snapshot.indexes.iter_mut().find(|index| index.name == index_name) {
                    index.protected = true;
                    index.reason = match contype.as_str() {
                        "p" => "由主键约束维护".to_owned(),
                        "f" => format!("外键 {name} 依赖此索引"),
                        _ => format!("由唯一约束 {name} 维护"),
                    };
                }
            }
        }
        if contype == "f" {
            if let Some(parsed) = parse_fk_def(&name, &definition) {
                snapshot.foreign_keys.push(parsed);
            }
        }
    }
    for column in &mut snapshot.columns {
        column.primary = snapshot.indexes.iter().any(|index| index.reason.contains("主键") && index.columns.iter().any(|item| item.name == column.name));
    }
    let incoming = client
        .query(
            "SELECT con.conname, n.nspname, c.relname, pg_get_constraintdef(con.oid) FROM pg_constraint con JOIN pg_class c ON c.oid = con.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace WHERE con.contype = 'f' AND con.confrelid = $1",
            &[&oid],
        )
        .await
        .map_err(|err| err.to_string())?;
    for row in &incoming {
        let name: String = row.get(0);
        let schema_name: String = row.get(1);
        let table_name: String = row.get(2);
        let definition: String = row.get(3);
        if let Some(parsed) = parse_fk_def(&name, &definition) {
            snapshot.incoming.push(IncomingForeignKey {
                name,
                table: table_name,
                schema: schema_name,
                columns: parsed.columns,
                ref_columns: parsed.ref_columns,
            });
        }
    }
    let triggers = client
        .query("SELECT tgname, pg_get_triggerdef(oid) FROM pg_trigger WHERE tgrelid = $1 AND NOT tgisinternal", &[&oid])
        .await
        .map_err(|err| err.to_string())?;
    for row in &triggers {
        snapshot.triggers.push(DependentSql { name: row.get(0), sql: row.get(1) });
    }
    let views = client
        .query("SELECT c.relname, pg_get_viewdef(c.oid) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('v','m')", &[&schema])
        .await
        .map_err(|err| err.to_string())?;
    for row in &views {
        let name: String = row.get(0);
        let sql: String = row.get(1);
        if mentions(&sql, table) {
            snapshot.views.push(DependentSql { name, sql });
        }
    }
    let count = client
        .query_one(&format!("SELECT COUNT(*) FROM {}", qualified_table(Engine::Postgres, database, schema, table)?), &[])
        .await
        .map_err(|err| err.to_string())?;
    snapshot.row_count = Some(count.get(0));
    snapshot.object_exists = true;
    if snapshot.kind == "view" {
        match postgres_view_definition(client, oid, database, schema, table, snapshot.materialized).await {
            Ok(sql) => snapshot.table_sql = sql,
            Err(message) => {
                snapshot.table_sql.clear();
                snapshot.definition_error = Some(message);
            }
        }
    } else {
        snapshot.table_sql = format!("-- {schema}.{table}");
    }
    Ok(snapshot)
}

async fn postgres_view_definition(
    client: &tokio_postgres::Client,
    oid: u32,
    database: &str,
    schema: &str,
    table: &str,
    materialized: bool,
) -> Result<String, String> {
    let row = client
        .query_one("SELECT pg_get_viewdef($1::oid, true)", &[&oid])
        .await
        .map_err(|err| err.to_string())?;
    let body: String = row.get(0);
    if body.trim().is_empty() {
        return Err("没有读到视图定义".to_owned());
    }
    let qualified = qualified_table(Engine::Postgres, database, schema, table)?;
    let body = body.trim().trim_end_matches(';').trim();
    if materialized {
        Ok(format!("CREATE MATERIALIZED VIEW {qualified} AS\n{body};"))
    } else {
        Ok(format!("CREATE OR REPLACE VIEW {qualified} AS\n{body};"))
    }
}

fn parse_fk_def(name: &str, definition: &str) -> Option<ForeignKey> {
    let lower = definition.to_ascii_lowercase();
    let key_at = lower.find("foreign key")?;
    let columns = paren_list(&definition[key_at + "foreign key".len()..])?;
    let ref_at = lower.find("references")?;
    let after = definition[ref_at + "references".len()..].trim();
    let (ref_schema, ref_table, ref_columns) = parse_reference(after)?;
    let on_delete = clause_action(definition, "on delete").unwrap_or_else(|| "NO ACTION".to_owned());
    let on_update = clause_action(definition, "on update").unwrap_or_else(|| "NO ACTION".to_owned());
    Some(ForeignKey { name: name.to_owned(), columns, ref_schema, ref_table, ref_columns, on_delete, on_update })
}

fn parse_reference(text: &str) -> Option<(String, String, Vec<String>)> {
    let paren = text.find('(')?;
    let head = text[..paren].trim().trim_matches('"');
    let mut parts = head.split('.').map(|part| part.trim().trim_matches('"').to_owned()).collect::<Vec<_>>();
    let ref_table = parts.pop()?;
    let ref_schema = parts.pop().unwrap_or_default();
    let ref_columns = paren_list(&text[paren..])?;
    Some((ref_schema, ref_table, ref_columns))
}

fn clause_action(definition: &str, clause: &str) -> Option<String> {
    let lower = definition.to_ascii_lowercase();
    let pos = lower.find(clause)?;
    let rest = definition[pos + clause.len()..].trim();
    let words = rest.split_whitespace().take(2).collect::<Vec<_>>();
    if words.first().map(|word| word.eq_ignore_ascii_case("SET")).unwrap_or(false) {
        Some(format!("SET {}", words.get(1).unwrap_or(&"").trim_matches(',').to_ascii_uppercase()))
    } else {
        Some(words.first()?.trim_matches(',').to_ascii_uppercase())
    }
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

fn mysql_string(row: &mysql_async::Row, index: usize) -> String {
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
