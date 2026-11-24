//! SQLite 的 DDL 方言：只能改 main、按官方 12 步重建表、会话级外键开关，以及 PRAGMA/ATTACH/维护等库级动作。

use crate::adapters::ddl::{AdminSections, DdlDialect, TableOptionSpec};
use crate::adapters::quote_ident;
use crate::connections::model::Engine;
use crate::connections::CatalogSession;
use crate::ddl::model::{
    Action, Column, ColumnDraft, ForeignKey, Plan, Request, Risk, Snapshot, Step, TableOptions, SQLITE_REBUILD_RELEASE_SQL, SQLITE_REBUILD_SAVEPOINT_SQL,
};
use crate::ddl::plan::{
    changed_names, column_referenced, column_sql, create_statement, drafts_change_shape, finish, identity_drafts, index_touches_change, default_changed,
    preservable_indexes, primary_key_change, push_fk_status, recreate_index_sql, renamed_any, same_type, Change,
};
use crate::ddl::sql::{mentions, valid_name};

use super::ddl_read;

pub(in crate::adapters) struct SqliteDdl;

/// 当前连接只打开了 main：其它库名不另开连接。
fn qualified(database: &str, table: &str) -> Result<String, String> {
    if !database.is_empty() && database != "main" {
        return Err(format!("当前连接打开的是 main，没有另开连接修改 {database}"));
    }
    Ok(quote_ident(Engine::Sqlite, table))
}

pub(crate) fn sqlite_at_least(version: &str, major: u64, minor: u64, patch: u64) -> bool {
    let mut parts = version.split(['.', '-', ' ']);
    let got = [
        parts.next().unwrap_or("0").parse::<u64>().unwrap_or(0),
        parts.next().unwrap_or("0").parse::<u64>().unwrap_or(0),
        parts.next().unwrap_or("0").parse::<u64>().unwrap_or(0),
    ];
    got >= [major, minor, patch]
}

#[async_trait::async_trait]
impl DdlDialect for SqliteDdl {
    /// SQLite 不限制标识符长度。
    fn max_index_name_len(&self) -> Option<usize> {
        None
    }

    fn qualified_table(&self, database: &str, _schema: &str, table: &str) -> Result<String, String> {
        qualified(database, table)
    }

    /// 建表（含重建用的临时表）只写表名。
    fn create_table_name(&self, _database: &str, _schema: &str, table: &str) -> Result<String, String> {
        Ok(quote_ident(Engine::Sqlite, table))
    }

    fn target_scope(&self, _database: &str, _schema: &str, table: &str) -> String {
        table.to_owned()
    }

    fn rule_text(&self) -> &'static str {
        "这些语句逐条执行，失败即停。已经成功的语句不撤销，后面的语句不执行。"
    }

    fn rebuild_rule_text(&self) -> &'static str {
        "SQLite 按官方步骤重建：先在事务外关闭外键检查，再在一个事务里建新表、复制数据、删除旧表并改回原名，然后做 foreign_key_check（外键完整性检查）。这只属于这次重建，不是所有数据库都能回滚。事务内失败会回滚这次重建；事务外改过的 foreign_keys 会恢复为会话原来的值。"
    }

    fn session_foreign_keys(&self) -> bool {
        true
    }

    fn add_not_null_without_default(&self, column: &str) -> Option<String> {
        Some(format!("SQLite 新增非空字段 {column} 必须有非 NULL 默认值。"))
    }

    fn inline_autoincrement_primary_key(&self) -> bool {
        true
    }

    fn finish_inline_autoincrement(&self, sql: &mut String) {
        if !sql.contains("AUTOINCREMENT") {
            sql.push_str(" PRIMARY KEY AUTOINCREMENT");
        }
    }

    fn create_table_suffix(&self, options: &TableOptions) -> String {
        let mut suffix = String::new();
        if options.without_rowid {
            suffix.push_str(" WITHOUT ROWID");
        }
        if options.strict {
            suffix.push_str(if options.without_rowid { ", STRICT" } else { " STRICT" });
        }
        suffix
    }

    fn table_option_spec(&self) -> TableOptionSpec {
        TableOptionSpec { without_rowid: Some("WITHOUT ROWID"), strict: Some("STRICT"), ..TableOptionSpec::default() }
    }

    fn alter_plan(&self, snapshot: &Snapshot, table: &str, drafts: &[ColumnDraft], changes: &[Change], target: String, mut risks: Vec<Risk>) -> Plan {
        // SQLite 不能用 ALTER TABLE 改主键：主键变了一律走重建，按新主键顺序建新表再复制数据。
        if primary_key_change(snapshot, drafts).is_some() {
            risks.insert(
                0,
                Risk {
                    level: "danger".to_owned(),
                    text: "改主键要重建整张表：按新定义建表、复制全部数据、删除旧表再改回原名。表越大耗时越长。".to_owned(),
                },
            );
            return sqlite_rebuild(snapshot, drafts, target, risks);
        }
        match sqlite_direct_alters(snapshot, table, changes) {
            SqlitePath::Direct(steps) => finish(snapshot.engine, false, target, Vec::new(), risks, steps),
            SqlitePath::Rebuild => sqlite_rebuild(snapshot, drafts, target, risks),
        }
    }

    fn drop_index_sql(&self, _snapshot: &Snapshot, index: &str) -> Result<String, String> {
        Ok(format!("DROP INDEX {index};"))
    }

    fn requires_reference_read(&self) -> bool {
        false
    }

    fn foreign_key_change_risk(&self, adding: bool) -> Option<&'static str> {
        Some(if adding { "SQLite 不能直接 ALTER TABLE 添加外键，需要按官方步骤重建表。" } else { "SQLite 需要重建表才能移除外键。" })
    }

    fn rebuild_with_foreign_keys(&self, snapshot: &Snapshot, keys: &[ForeignKey], target: &str, risks: &[Risk]) -> Option<Plan> {
        Some(sqlite_rebuild_with(snapshot, &identity_drafts(snapshot), keys, target.to_owned(), risks.to_vec()))
    }

    fn drop_referenced_note(&self, live: Option<bool>) -> &'static str {
        if live == Some(false) {
            "不会添加 CASCADE。当前 foreign_keys = OFF 时 SQLite 仍可能删除这张表，引用方会留下无效引用。"
        } else {
            "不会添加 CASCADE。被引用时数据库会拒绝删除。"
        }
    }

    fn truncate_note(&self) -> &'static str {
        "SQLite 没有 TRUNCATE，这里用 DELETE FROM 逐行删除；AUTOINCREMENT 的计数不会重置，删除触发器会触发。"
    }

    fn truncate_referenced_note(&self, live: Option<bool>) -> &'static str {
        if live == Some(true) {
            "当前 foreign_keys = ON：引用方还有数据时删除可能被拒绝，或按外键规则级联。"
        } else {
            "当前 foreign_keys 未开启：删除会成功，引用方会留下无效引用。"
        }
    }

    fn truncate_sql(&self, qualified: &str) -> String {
        format!("DELETE FROM {qualified};")
    }

    fn rename_blocker(&self, table: &str, wanted: &str) -> Option<String> {
        // SQLite 的名字不分大小写，只改大小写会被数据库当成重名拒绝；这不是被其它对象占用。
        wanted
            .eq_ignore_ascii_case(table)
            .then(|| format!("SQLite 不能只改表名的大小写（{table} → {wanted}）。可以先改成另一个临时名，再改成 {wanted}。"))
    }

    fn replace_view_blocked_by_dependents(&self) -> bool {
        true
    }

    fn view_qualifiers<'a>(&self, snapshot: &'a Snapshot) -> Vec<&'a str> {
        vec![snapshot.database.as_str(), "main"]
    }

    fn plan_object_action(&self, snapshot: &Snapshot, action: &Action) -> Option<Plan> {
        object_action(snapshot, action, true)
    }

    fn refuse_object_action(&self, snapshot: &Snapshot, action: &Action) -> Option<Plan> {
        object_action(snapshot, action, false)
    }

    fn admin_sections(&self) -> AdminSections {
        AdminSections { settings: true, attached: true, maintenance: true, ..AdminSections::default() }
    }

    fn table_option_labels(&self, snapshot: &Snapshot) -> Vec<&'static str> {
        let mut labels = Vec::new();
        if snapshot.strict {
            labels.push("STRICT");
        }
        if snapshot.without_rowid {
            labels.push("WITHOUT ROWID");
        }
        labels
    }

    fn matches_session(&self, session: &CatalogSession) -> bool {
        matches!(session, CatalogSession::Sqlite { .. })
    }

    async fn load_snapshot(&self, session: &CatalogSession, request: &Request, table: &str) -> Result<Snapshot, String> {
        let CatalogSession::Sqlite { pool, foreign_keys } = session else {
            return Err(crate::ddl::introspect::SESSION_MISMATCH.to_owned());
        };
        ddl_read::load_sqlite(pool, &request.database, table, *foreign_keys).await
    }

    async fn name_taken(&self, session: &CatalogSession, snapshot: &Snapshot, name: &str) -> Result<bool, String> {
        let CatalogSession::Sqlite { pool, .. } = session else {
            return Err(crate::ddl::introspect::SESSION_MISMATCH.to_owned());
        };
        ddl_read::name_taken(pool, &snapshot.name, name).await
    }

    async fn count(&self, session: &CatalogSession, sql: &str) -> Result<i64, String> {
        let CatalogSession::Sqlite { pool, .. } = session else {
            return Err(crate::ddl::introspect::SESSION_MISMATCH.to_owned());
        };
        ddl_read::count(pool, sql).await
    }

    async fn read_session_state(&self, session: &CatalogSession, snapshot: &mut Snapshot) -> Result<(), String> {
        let CatalogSession::Sqlite { pool, foreign_keys } = session else {
            return Ok(());
        };
        snapshot.recorded_foreign_keys = *foreign_keys;
        ddl_read::read_session_state(pool, snapshot).await
    }

    async fn read_object_state(&self, session: &CatalogSession, request: &Request, snapshot: &mut Snapshot) -> Result<(), String> {
        if !matches!(request.action, Action::SetPragma { .. } | Action::AttachDatabase { .. } | Action::Vacuum | Action::Reindex { .. }) {
            return Ok(());
        }
        let CatalogSession::Sqlite { pool, foreign_keys } = session else {
            return Err(crate::ddl::introspect::SESSION_MISMATCH.to_owned());
        };
        let state = ddl_read::sqlite_state(pool).await?;
        snapshot.recorded_foreign_keys = *foreign_keys;
        snapshot.live_foreign_keys = state.foreign_keys;
        snapshot.pragmas = state.pragmas;
        snapshot.attached = state.attached;
        Ok(())
    }

    async fn maintenance_note(&self, session: &CatalogSession, action: &Action) -> Option<String> {
        let CatalogSession::Sqlite { pool, .. } = session else {
            return None;
        };
        ddl_read::maintenance_note(pool, action).await
    }

    fn sets_session_foreign_keys(&self, sql: &str, enabled: bool) -> bool {
        ddl_read::pragma_foreign_keys_is(sql, enabled)
    }
}

/// SQLite 的库级动作。`supported` 为假时是别的引擎请求了它：给出同一个动作的拒绝计划。
fn object_action(snapshot: &Snapshot, action: &Action, supported: bool) -> Option<Plan> {
    // 活动外层事务里不能执行的动作先明确拒绝：不另开会话，也不用一条“成功”的 no-op 冒充完成。
    if snapshot.transaction == crate::transaction::TransactionState::Active {
        if let Some(reason) = transaction_refusal(action) {
            let target = match action {
                Action::Vacuum => "sqlite / vacuum".to_owned(),
                Action::SetForeignKeys { .. } => "sqlite / foreign_keys".to_owned(),
                Action::SetPragma { pragma, .. } => format!("sqlite / pragma {}", pragma.trim().to_ascii_lowercase()),
                _ => String::new(),
            };
            return Some(finish(snapshot.engine, false, target, vec![reason.to_owned()], Vec::new(), Vec::new()));
        }
    }
    Some(match action {
        Action::SetForeignKeys { enabled } => plan_set_fk(snapshot, *enabled, supported),
        Action::SetPragma { pragma, value } => plan_set_pragma(snapshot, pragma, value, supported),
        Action::AttachDatabase { path, name } => plan_attach_database(snapshot, path, name, supported),
        Action::Vacuum => plan_vacuum(snapshot, supported),
        Action::Reindex { target } => plan_reindex(snapshot, target.as_deref(), supported),
        _ => return None,
    })
}

/// 活动外层事务里 SQLite 不能执行的结构动作：明确拒绝，不另开会话绕过（R4）。
fn transaction_refusal(action: &Action) -> Option<&'static str> {
    match action {
        Action::Vacuum => Some("SQLite 不能在事务里执行 VACUUM：先在当前标签提交或回滚事务，不会另开会话绕过。"),
        Action::SetForeignKeys { .. } => Some("活动事务里 PRAGMA foreign_keys 不生效：先提交或回滚事务再改外键开关，不会另开会话绕过。"),
        Action::SetPragma { pragma, .. } if pragma.trim().eq_ignore_ascii_case("journal_mode") => {
            Some("活动事务里不能改变 journal_mode：先提交或回滚事务，不会另开会话绕过。")
        }
        _ => None,
    }
}

fn plan_set_fk(snapshot: &Snapshot, enabled: bool, supported: bool) -> Plan {
    let target = "sqlite / foreign_keys".to_owned();
    let mut risks = vec![Risk {
        level: "info".to_owned(),
        text: "只改变本次已打开连接的 foreign_keys。不会清理已有数据，也不会把外键说成已经验证成功。".to_owned(),
    }];
    if !supported {
        return finish(snapshot.engine, false, target, vec!["只有 SQLite 用连接上的 foreign_keys 控制外键是否生效。".to_owned()], risks, Vec::new());
    }
    push_fk_status(snapshot, &mut risks);
    let value = if enabled { "ON" } else { "OFF" };
    match snapshot.live_foreign_keys {
        None => finish(snapshot.engine, false, target, vec!["没有读到 foreign_keys，没有生成语句。".to_owned()], risks, Vec::new()),
        Some(current) if current == enabled => {
            finish(snapshot.engine, false, target, vec![format!("当前连接 foreign_keys 已经是 {value}。")], risks, Vec::new())
        }
        Some(_) => finish(
            snapshot.engine,
            false,
            target,
            Vec::new(),
            risks,
            vec![Step { sql: format!("PRAGMA foreign_keys = {value};"), label: format!("设置 foreign_keys = {value}") }],
        ),
    }
}

fn plan_set_pragma(snapshot: &Snapshot, pragma: &str, value: &str, supported: bool) -> Plan {
    let name = pragma.trim().to_ascii_lowercase();
    let target = format!("sqlite / pragma {name}");
    if !supported {
        return finish(snapshot.engine, false, target, vec!["只有 SQLite 用 PRAGMA 改这些开关，这里不生成语句。".to_owned()], Vec::new(), Vec::new());
    }
    if name == "foreign_keys" {
        return finish(
            snapshot.engine,
            false,
            target,
            vec!["foreign_keys 走外键开关动作（SetForeignKeys），这里不重复生成同一条 PRAGMA。".to_owned()],
            Vec::new(),
            Vec::new(),
        );
    }
    let mut blocked = Vec::new();
    let mut risks = Vec::new();
    let normalized = match name.as_str() {
        "journal_mode" => {
            let mode = value.trim().to_ascii_lowercase();
            const MODES: [&str; 6] = ["delete", "truncate", "persist", "memory", "wal", "off"];
            if !MODES.contains(&mode.as_str()) {
                blocked.push("journal_mode 只能是 delete、truncate、persist、memory、wal 或 off。".to_owned());
            }
            risks.push(Risk { level: "info".to_owned(), text: "journal_mode 写进数据库文件头，重新打开这个文件后仍然是这个模式。".to_owned() });
            if mode == "wal" {
                risks.push(Risk {
                    level: "warn".to_owned(),
                    text: "WAL 会在库文件旁边多出 -wal 与 -shm 文件，需要文件系统支持共享内存。".to_owned(),
                });
            }
            if mode == "off" {
                risks.push(Risk { level: "warn".to_owned(), text: "关掉日志后崩溃时无法回滚未完成的事务。".to_owned() });
            }
            if blocked.is_empty() { mode.to_ascii_uppercase() } else { String::new() }
        }
        "user_version" => {
            let number = value.trim().parse::<i64>().ok().filter(|number| (0..=2_147_483_647).contains(number));
            match number {
                Some(number) => number.to_string(),
                None => {
                    blocked.push("user_version 只接受 0 到 2147483647 的整数。".to_owned());
                    String::new()
                }
            }
        }
        _ => {
            blocked.push("这里只改 journal_mode 与 user_version，其它 PRAGMA 不放行。".to_owned());
            String::new()
        }
    };
    if blocked.is_empty() {
        if let Some(current) = snapshot.pragmas.get(&name) {
            if current.eq_ignore_ascii_case(normalized.trim()) {
                blocked.push(format!("{name} 已经是 {normalized}，没有生成语句。"));
            }
        }
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    let sql = format!("PRAGMA {name} = {normalized};");
    finish(
        snapshot.engine,
        false,
        target,
        Vec::new(),
        risks,
        vec![Step { label: format!("设置 {name} = {normalized}"), sql }],
    )
}

fn plan_attach_database(snapshot: &Snapshot, path: &str, name: &str, supported: bool) -> Plan {
    let trimmed_name = name.trim();
    let target = format!("sqlite / attach {trimmed_name}");
    if !supported {
        return finish(snapshot.engine, false, target, vec!["只有 SQLite 用 ATTACH 附加数据库文件，这里不生成语句。".to_owned()], Vec::new(), Vec::new());
    }
    let trimmed_path = path.trim();
    let mut blocked = Vec::new();
    if trimmed_path.is_empty() {
        blocked.push("要附加的数据库文件路径不能为空。".to_owned());
    } else if trimmed_path.chars().any(char::is_control) {
        blocked.push("文件路径里不能有控制字符。".to_owned());
    } else if trimmed_path.len() > 1024 {
        blocked.push("文件路径太长。".to_owned());
    }
    if !valid_name(trimmed_name) {
        blocked.push("附加库的名字无效。".to_owned());
    } else if trimmed_name.eq_ignore_ascii_case("main") || trimmed_name.eq_ignore_ascii_case("temp") {
        blocked.push(format!("{trimmed_name} 是 SQLite 保留的库名，不能用来附加。"));
    } else if snapshot.attached.iter().any(|item| item.name.eq_ignore_ascii_case(trimmed_name)) {
        blocked.push(format!("{trimmed_name} 已经附加在这个连接上，没有生成语句。"));
    }
    let mut risks = vec![
        Risk { level: "warn".to_owned(), text: "路径上没有库文件时 SQLite 会新建一个空库；这里不检查文件是否已经存在。".to_owned() },
        Risk { level: "info".to_owned(), text: "ATTACH 只对当前连接生效，断开连接后不保留。".to_owned() },
    ];
    if let Some(same) = snapshot.attached.iter().find(|item| !item.path.is_empty() && item.path == trimmed_path) {
        risks.push(Risk {
            level: "warn".to_owned(),
            text: format!("同一个文件已经作为 {} 附加过；再加一次会在写入时互相干扰。", same.name),
        });
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    let literal = trimmed_path.replace('\'', "''");
    let sql = format!("ATTACH DATABASE '{literal}' AS {};", quote_ident(Engine::Sqlite, trimmed_name));
    finish(
        snapshot.engine,
        false,
        target,
        Vec::new(),
        risks,
        vec![Step { label: format!("附加 {trimmed_name}"), sql }],
    )
}

fn plan_vacuum(snapshot: &Snapshot, supported: bool) -> Plan {
    let target = "sqlite / vacuum".to_owned();
    let risks = vec![
        Risk {
            level: "warn".to_owned(),
            text: "VACUUM 会重写整个数据库文件，需要额外的临时磁盘空间；不确认执行就不会写入。".to_owned(),
        },
        Risk { level: "info".to_owned(), text: "VACUUM 不能在事务里执行，这里单条执行，不改表结构也不改数据。".to_owned() },
    ];
    if !supported {
        return finish(snapshot.engine, false, target, vec!["维护动作只对 SQLite 提供；其它引擎请在查询里自己写 VACUUM。".to_owned()], risks, Vec::new());
    }
    finish(
        snapshot.engine,
        false,
        target,
        Vec::new(),
        risks,
        vec![Step { sql: "VACUUM;".to_owned(), label: "整理数据库文件".to_owned() }],
    )
}

fn plan_reindex(snapshot: &Snapshot, target_name: Option<&str>, supported: bool) -> Plan {
    let wanted = target_name.map(str::trim).filter(|item| !item.is_empty());
    let target = match wanted {
        Some(name) => format!("sqlite / reindex {name}"),
        None => "sqlite / reindex".to_owned(),
    };
    let risks = vec![
        Risk { level: "info".to_owned(), text: "REINDEX 只重建索引，不改表里的数据。".to_owned() },
        Risk { level: "warn".to_owned(), text: "大表重建索引期间会占用写锁，其它写入要等它结束。".to_owned() },
    ];
    if !supported {
        return finish(snapshot.engine, false, target, vec!["维护动作只对 SQLite 提供；其它引擎请在查询里自己写 REINDEX。".to_owned()], risks, Vec::new());
    }
    let mut blocked = Vec::new();
    if let Some(name) = wanted {
        if !valid_name(name) {
            blocked.push("要重建的索引或表名无效。".to_owned());
        }
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    let sql = match wanted {
        Some(name) => format!("REINDEX {};", quote_ident(Engine::Sqlite, name)),
        None => "REINDEX;".to_owned(),
    };
    let label = match wanted {
        Some(name) => format!("重建 {name} 的索引"),
        None => "重建当前库的索引".to_owned(),
    };
    finish(snapshot.engine, false, target, Vec::new(), risks, vec![Step { sql, label }])
}

fn sqlite_rebuild(snapshot: &Snapshot, drafts: &[ColumnDraft], target: String, mut risks: Vec<Risk>) -> Plan {
    risks.insert(
        0,
        Risk {
            level: "warn".to_owned(),
            text: "需要重建表。SQLite 不能直接完成这项修改，将按官方 12 步新建表、复制数据并替换原表。".to_owned(),
        },
    );
    sqlite_rebuild_with(snapshot, drafts, &snapshot.foreign_keys, target, risks)
}

fn sqlite_rebuild_with(snapshot: &Snapshot, drafts: &[ColumnDraft], foreign_keys: &[ForeignKey], target: String, mut risks: Vec<Risk>) -> Plan {
    if let Some(message) = rebuild_blocker(snapshot, drafts) {
        return finish(snapshot.engine, true, target, vec![message], risks, Vec::new());
    }
    let kept_indexes = preservable_indexes(snapshot, drafts);
    for index in &snapshot.indexes {
        if index.protected && index.reason.contains("主键") {
            continue;
        }
        if !kept_indexes.iter().any(|kept| kept.name == index.name) && index_touches_change(index, snapshot, drafts) {
            risks.push(Risk {
                level: "warn".to_owned(),
                text: format!("索引 {} 引用了被删除或无法原样保留的列，重建时不会保留它。", index.name),
            });
        }
    }
    let temp = format!("_rill_new_{}", snapshot.name);
    let qualified_temp = quote_ident(Engine::Sqlite, &temp);
    let qualified_old = quote_ident(Engine::Sqlite, &snapshot.name);
    let options = TableOptions { without_rowid: snapshot.without_rowid, strict: snapshot.strict, ..TableOptions::default() };
    let create = match create_statement(Engine::Sqlite, &snapshot.database, &snapshot.schema, &temp, drafts, foreign_keys, snapshot.autoincrement, &options) {
        Ok(sql) => sql,
        Err(message) => return finish(snapshot.engine, true, target, vec![message], risks, Vec::new()),
    };
    let kept: Vec<(&ColumnDraft, &Column)> = drafts
        .iter()
        .filter_map(|draft| {
            let original_name = draft.original.as_deref()?;
            if draft.dropped {
                return None;
            }
            snapshot.columns.iter().find(|column| column.name == original_name).map(|column| (draft, column))
        })
        .collect();
    if kept.is_empty() {
        return finish(snapshot.engine, true, target, vec!["没有可以可靠复制的原有列，因此阻止重建。".to_owned()], risks, Vec::new());
    }
    let insert_cols = kept.iter().map(|(draft, _)| quote_ident(Engine::Sqlite, draft.name.trim())).collect::<Vec<_>>().join(", ");
    let select_cols = kept.iter().map(|(_, column)| quote_ident(Engine::Sqlite, &column.name)).collect::<Vec<_>>().join(", ");
    let copy_label = match snapshot.row_count {
        Some(count) => format!("复制 {count} 行"),
        None => "复制保留列的数据".to_owned(),
    };
    let in_transaction = snapshot.transaction == crate::transaction::TransactionState::Active;
    let restore = match snapshot.live_foreign_keys {
        Some(true) => "ON",
        Some(false) => "OFF",
        None if in_transaction => "ON",
        None => {
            return finish(
                snapshot.engine,
                true,
                target,
                vec!["不知道本次会话原来的 foreign_keys 设置，不能在重建后恢复，因此没有执行。".to_owned()],
                risks,
                Vec::new(),
            );
        }
    };
    let mut steps = Vec::new();
    if in_transaction {
        // 活动外层事务里不能另开事务，也不能在事务内改 foreign_keys：重建边界改用内部保存点。
        steps.push(Step { sql: SQLITE_REBUILD_SAVEPOINT_SQL.to_owned(), label: "开始重建保存点".to_owned() });
    } else {
        steps.push(Step { sql: "PRAGMA foreign_keys = OFF;".to_owned(), label: "在事务外关闭外键检查".to_owned() });
        steps.push(Step { sql: "BEGIN;".to_owned(), label: "开始重建事务".to_owned() });
    }
    steps.push(Step { sql: create, label: "按新定义创建新表".to_owned() });
    steps.push(Step {
        sql: format!("INSERT INTO {qualified_temp} ({insert_cols}) SELECT {select_cols} FROM {qualified_old};"),
        label: copy_label,
    });
    steps.push(Step { sql: format!("DROP TABLE {qualified_old};"), label: "删除旧表".to_owned() });
    steps.push(Step { sql: format!("ALTER TABLE {qualified_temp} RENAME TO {qualified_old};"), label: "新表改回原名".to_owned() });
    for index in &kept_indexes {
        match recreate_index_sql(snapshot, index, drafts) {
            Ok(sql) => steps.push(Step { sql, label: format!("重建索引 {}", index.name) }),
            Err(message) => return finish(snapshot.engine, true, target, vec![message], risks, Vec::new()),
        }
    }
    for trigger in &snapshot.triggers {
        if let Some(sql) = trigger.sql.strip_prefix("").filter(|sql| !sql.trim().is_empty()) {
            let statement = if sql.trim_end().ends_with(';') { sql.trim().to_owned() } else { format!("{};", sql.trim()) };
            steps.push(Step { sql: statement, label: format!("重建触发器 {}", trigger.name) });
        }
    }
    steps.push(Step {
        sql: format!("PRAGMA foreign_key_check({qualified_old});"),
        label: "检查外键；有违规则停止并回滚这次重建".to_owned(),
    });
    if in_transaction {
        steps.push(Step { sql: SQLITE_REBUILD_RELEASE_SQL.to_owned(), label: "释放重建保存点（外层事务仍未提交）".to_owned() });
        risks.push(Risk {
            level: "warn".to_owned(),
            text: "活动事务里不能在事务内关闭外键检查：重建按当前外键设置执行；被其他表引用时数据库可能拒绝删除旧表。失败只回滚到重建保存点，不会撤销此前已经应用的修改。".to_owned(),
        });
    } else {
        steps.push(Step { sql: "COMMIT;".to_owned(), label: "提交重建事务".to_owned() });
        steps.push(Step {
            sql: format!("PRAGMA foreign_keys = {restore};"),
            label: format!("恢复会话原来的 foreign_keys = {restore}"),
        });
        if snapshot.live_foreign_keys == Some(false) {
            risks.push(Risk {
                level: "info".to_owned(),
                text: "当前连接 foreign_keys = OFF。重建结束后仍恢复为 OFF，外键定义不会因此变成强制检查。".to_owned(),
            });
        }
    }
    let mut plan = finish(snapshot.engine, true, target, Vec::new(), risks, steps);
    if in_transaction {
        plan.rule = "活动事务里的 SQLite 重建：在内部保存点里建新表、复制数据、删除旧表并改回原名，然后做 foreign_key_check（外键完整性检查）。不能关闭外键检查；失败只回滚到这个保存点，不会撤销此前已经应用的修改。重建结果随当前事务一起提交或回滚。".to_owned();
    }
    plan
}

fn rebuild_blocker(snapshot: &Snapshot, drafts: &[ColumnDraft]) -> Option<String> {
    if snapshot.table_sql.trim().is_empty() {
        return Some("读不到原来的建表语句，不能确认索引和约束能否保留，因此阻止重建。".to_owned());
    }
    if snapshot.columns.iter().any(|column| column.generated) || snapshot.table_sql.to_ascii_uppercase().contains("GENERATED ALWAYS") {
        return Some("表含生成列，重建不能可靠保留这些列，因此阻止执行。".to_owned());
    }
    if snapshot.has_check {
        return Some("表含 CHECK 约束，重建不能可靠保留该约束，因此阻止执行。".to_owned());
    }
    if snapshot.has_collate {
        return Some("表含排序规则，重建不能可靠保留该规则，因此阻止执行。".to_owned());
    }
    if snapshot.without_rowid && !drafts.iter().any(|draft| draft.primary && !draft.dropped) {
        return Some("WITHOUT ROWID 表必须保留主键，重建不能可靠保留行标识，因此阻止执行。".to_owned());
    }
    let changed = changed_names(snapshot, drafts);
    for trigger in &snapshot.triggers {
        if changed.iter().any(|name| mentions(&trigger.sql, name)) {
            return Some(format!("触发器 {} 引用了将改变的列，无法可靠改写，因此阻止重建。", trigger.name));
        }
        if trigger.sql.trim().is_empty() {
            return Some(format!("读不到触发器 {} 的定义，不能可靠重建。", trigger.name));
        }
    }
    for view in &snapshot.views {
        if changed.iter().any(|name| mentions(&view.sql, name)) || changed.is_empty() && mentions(&view.sql, &snapshot.name) && drafts_change_shape(snapshot, drafts) {
            return Some(format!("视图 {} 引用了将改变的结构，无法可靠改写，因此阻止重建。", view.name));
        }
    }
    for incoming in &snapshot.incoming {
        if incoming.ref_columns.iter().any(|name| changed.iter().any(|changed_name| changed_name == name)) {
            return Some(format!("外键 {} 引用了将改变的列，重建不能可靠保留该约束。", incoming.name));
        }
    }
    for index in &snapshot.indexes {
        if index.protected && index.columns.iter().any(|column| snapshot.columns.iter().any(|item| item.name == column.name && item.primary)) {
            continue;
        }
        let referenced: Vec<&str> = index.columns.iter().map(|column| column.name.as_str()).collect();
        let dropped = referenced.iter().any(|name| drafts.iter().any(|draft| draft.dropped && draft.original.as_deref() == Some(*name)));
        let renamed = referenced.iter().any(|name| {
            drafts.iter().any(|draft| draft.original.as_deref() == Some(*name) && !draft.dropped && draft.name.trim() != *name)
        });
        if (index.partial || index.expression) && renamed {
            return Some(format!("索引 {} 含表达式或部分条件，列被重命名后无法可靠改写。", index.name));
        }
        if (index.partial || index.expression) && !dropped && index.sql.as_deref().unwrap_or("").trim().is_empty() {
            return Some(format!("读不到索引 {} 的原始语句，不能可靠保留。", index.name));
        }
        if index.expression && index.columns.is_empty() && !dropped
            && renamed_any(drafts) {
                return Some(format!("表达式索引 {} 在列重命名后无法可靠改写。", index.name));
            }
    }
    None
}

enum SqlitePath {
    Direct(Vec<Step>),
    Rebuild,
}

fn sqlite_direct_alters(snapshot: &Snapshot, table: &str, changes: &[Change]) -> SqlitePath {
    let mut steps = Vec::new();
    for change in changes {
        match change {
            Change::Add(draft) => {
                if draft.primary {
                    return SqlitePath::Rebuild;
                }
                if !draft.nullable && !draft.default.has_value() {
                    return SqlitePath::Rebuild;
                }
                let qualified = match qualified(&snapshot.database, table) {
                    Ok(name) => name,
                    Err(_) => return SqlitePath::Rebuild,
                };
                let definition = match column_sql(Engine::Sqlite, draft, false) {
                    Ok(sql) => sql,
                    Err(_) => return SqlitePath::Rebuild,
                };
                steps.push(Step {
                    sql: format!("ALTER TABLE {qualified} ADD COLUMN {definition};"),
                    label: format!("新增字段 {}", draft.name.trim()),
                });
            }
            Change::Drop(column) => {
                if column_referenced(snapshot, &column.name) {
                    return SqlitePath::Rebuild;
                }
                let qualified = match qualified(&snapshot.database, table) {
                    Ok(name) => name,
                    Err(_) => return SqlitePath::Rebuild,
                };
                steps.push(Step {
                    sql: format!("ALTER TABLE {qualified} DROP COLUMN {};", quote_ident(Engine::Sqlite, &column.name)),
                    label: format!("删除字段 {}", column.name),
                });
            }
            Change::Modify { original, draft } => {
                let rename = draft.name.trim() != original.name;
                let type_change = !same_type(&draft.type_name, &original.type_name);
                let null_change = draft.nullable != original.nullable;
                let default_change = default_changed(Engine::Sqlite, original, draft);
                if type_change || default_change {
                    return SqlitePath::Rebuild;
                }
                let qualified = match qualified(&snapshot.database, table) {
                    Ok(name) => name,
                    Err(_) => return SqlitePath::Rebuild,
                };
                if rename {
                    steps.push(Step {
                        sql: format!(
                            "ALTER TABLE {qualified} RENAME COLUMN {} TO {};",
                            quote_ident(Engine::Sqlite, &original.name),
                            quote_ident(Engine::Sqlite, draft.name.trim())
                        ),
                        label: format!("重命名 {} → {}", original.name, draft.name.trim()),
                    });
                }
                if null_change {
                    if !nullability_is_direct(snapshot, original) {
                        return SqlitePath::Rebuild;
                    }
                    let verb = if draft.nullable { "DROP NOT NULL" } else { "SET NOT NULL" };
                    steps.push(Step {
                        sql: format!(
                            "ALTER TABLE {qualified} ALTER COLUMN {} {verb};",
                            quote_ident(Engine::Sqlite, draft.name.trim())
                        ),
                        label: if draft.nullable { format!("去掉 NOT NULL {}", draft.name.trim()) } else { format!("设置 NOT NULL {}", draft.name.trim()) },
                    });
                }
            }
        }
    }
    SqlitePath::Direct(steps)
}

fn nullability_is_direct(snapshot: &Snapshot, column: &Column) -> bool {
    sqlite_at_least(&snapshot.sqlite_version, DIRECT_NOT_NULL_VERSION.0, DIRECT_NOT_NULL_VERSION.1, DIRECT_NOT_NULL_VERSION.2)
        && !column.generated
        && !column.primary
}

const DIRECT_NOT_NULL_VERSION: (u64, u64, u64) = (3, 53, 0);
