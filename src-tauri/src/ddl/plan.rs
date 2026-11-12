use super::model::{
    fk_actions, rule_text, Action, Column, ColumnDraft, Engine, ForeignKey, ForeignKeyDraft, Index, Plan, Risk, SequenceState, Snapshot, Step,
};
use super::sql::{mentions, non_null_default, qualified_table, quote_ident, sqlite_at_least, valid_default, valid_name, valid_type};

const DIRECT_NOT_NULL_VERSION: (u64, u64, u64) = (3, 53, 0);

pub fn plan(snapshot: &Snapshot, action: &Action) -> Plan {
    match action {
        Action::CreateTable { name, columns } => plan_create(snapshot, name, columns),
        Action::AlterColumns { table, columns } => plan_alter(snapshot, table, columns),
        Action::CreateIndex { table, name, unique, columns } => plan_create_index(snapshot, table, name, *unique, columns),
        Action::DropIndex { table, name } => plan_drop_index(snapshot, table, name),
        Action::CreateForeignKey { table, foreign_key } => plan_create_fk(snapshot, table, foreign_key),
        Action::DropForeignKey { table, name } => plan_drop_fk(snapshot, table, name),
        Action::DropTable { table } => plan_drop_table(snapshot, table),
        Action::DropView { view } => plan_drop_view(snapshot, view),
        Action::TruncateTable { table } => plan_truncate(snapshot, table),
        Action::RenameTable { table, new_name } => plan_rename(snapshot, table, new_name),
        Action::SetForeignKeys { enabled } => plan_set_fk(snapshot, *enabled),
        Action::ReplaceView { table, definition } => plan_replace_view(snapshot, table, definition),
        Action::AlterSequence { sequence, start, increment, min_value, max_value, cycle } => plan_alter_sequence(
            snapshot,
            sequence,
            &SequenceState { start: *start, increment: *increment, min_value: *min_value, max_value: *max_value, cycle: *cycle },
        ),
        Action::AddEnumValue { enum_name, value, before } => plan_add_enum_value(snapshot, enum_name, value, before.as_deref()),
        Action::AlterDomain { domain, default_sql, not_null } => plan_alter_domain(snapshot, domain, default_sql.as_deref(), *not_null),
        Action::SetEventEnabled { event, enabled } => plan_set_event_enabled(snapshot, event, *enabled),
        Action::SetPragma { pragma, value } => plan_set_pragma(snapshot, pragma, value),
        Action::AttachDatabase { path, name } => plan_attach_database(snapshot, path, name),
        Action::Vacuum => plan_vacuum(snapshot),
        Action::Reindex { target } => plan_reindex(snapshot, target.as_deref()),
    }
}

pub fn display_sql(snapshot: &Snapshot) -> String {
    if snapshot.kind != "table" {
        return snapshot.table_sql.clone();
    }
    let columns: Vec<ColumnDraft> = snapshot
        .columns
        .iter()
        .map(|column| ColumnDraft {
            original: Some(column.name.clone()),
            name: column.name.clone(),
            type_name: column.type_name.clone(),
            nullable: column.nullable,
            default_sql: column.default_sql.clone().unwrap_or_default(),
            primary: column.primary,
            auto: column.auto,
            dropped: false,
        })
        .collect();
    match create_statement(snapshot.engine, &snapshot.database, &snapshot.schema, &snapshot.name, &columns, &snapshot.foreign_keys, snapshot.autoincrement, snapshot.without_rowid, snapshot.strict) {
        Ok(sql) => sql,
        Err(message) => message,
    }
}

fn plan_create(snapshot: &Snapshot, name: &str, columns: &[ColumnDraft]) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, name);
    let mut blocked = validate_columns(snapshot.engine, columns, true);
    if !valid_name(name) {
        blocked.push("表名无效。".to_owned());
    }
    if snapshot.object_exists {
        blocked.push(format!("表 {name} 已经存在。不会覆盖已有表或数据。"));
    }
    if columns.iter().all(|column| column.dropped) {
        blocked.push("至少需要一个字段。".to_owned());
    }
    let mut risks = vec![Risk {
        level: "info".to_owned(),
        text: "创建一张新表。同名对象已存在时数据库会报错，不会覆盖已有表或数据。".to_owned(),
    }];
    if !columns.iter().any(|column| column.primary && !column.dropped) {
        risks.push(Risk {
            level: "warn".to_owned(),
            text: "没有主键：之后可以新增行，但不能在表格里修改或删除已有行。".to_owned(),
        });
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    let autoincrement = columns.iter().any(|column| !column.dropped && column.primary && column.auto);
    match create_statement(snapshot.engine, &snapshot.database, &snapshot.schema, name.trim(), columns, &[], autoincrement, false, false) {
        Ok(sql) => finish(snapshot.engine, false, target, Vec::new(), risks, vec![Step { label: format!("创建 {name}"), sql }]),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

fn plan_alter(snapshot: &Snapshot, table: &str, drafts: &[ColumnDraft]) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, table);
    if let Some(message) = guard_table(snapshot, table) {
        return finish(snapshot.engine, false, target, vec![message], Vec::new(), Vec::new());
    }
    let mut blocked = validate_columns(snapshot.engine, drafts, false);
    blocked.extend(alter_precheck(snapshot, drafts));
    let risks = alter_risks(snapshot, drafts);
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    let changes = diff_columns(snapshot, drafts);
    if changes.is_empty() {
        return finish(snapshot.engine, false, target, vec!["没有字段变更。".to_owned()], risks, Vec::new());
    }
    if snapshot.engine == Engine::Sqlite {
        match sqlite_direct_alters(snapshot, table, &changes) {
            SqlitePath::Direct(steps) => return finish(snapshot.engine, false, target, Vec::new(), risks, steps),
            SqlitePath::Rebuild => return sqlite_rebuild(snapshot, drafts, target, risks),
        }
    }
    match remote_alters(snapshot, table, &changes) {
        Ok(steps) => finish(snapshot.engine, false, target, Vec::new(), risks, steps),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

fn plan_create_index(snapshot: &Snapshot, table: &str, name: &str, unique: bool, columns: &[super::model::IndexColumnDraft]) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, table);
    if let Some(message) = guard_table(snapshot, table) {
        return finish(snapshot.engine, false, target, vec![message], Vec::new(), Vec::new());
    }
    let mut blocked = Vec::new();
    if !valid_name(name) {
        blocked.push("索引名称无效。".to_owned());
    }
    if snapshot.indexes.iter().any(|index| index.name.eq_ignore_ascii_case(name.trim())) {
        blocked.push(format!("索引 {name} 已经存在，不会覆盖。"));
    }
    if columns.is_empty() {
        blocked.push("索引至少需要一列。".to_owned());
    }
    let mut seen = Vec::new();
    for column in columns {
        if !snapshot.columns.iter().any(|item| item.name == column.name) {
            blocked.push(format!("列 {} 不在表 {table} 中。", column.name));
        }
        if seen.iter().any(|item: &String| item == &column.name) {
            blocked.push(format!("列 {} 在索引里重复。", column.name));
        }
        seen.push(column.name.clone());
    }
    let order = columns.iter().map(|column| format!("{} {}", column.name, if column.descending { "DESC" } else { "ASC" })).collect::<Vec<_>>().join(" → ");
    let mut risks = vec![Risk {
        level: "info".to_owned(),
        text: if unique { format!("按 {order} 的顺序创建唯一索引。") } else { format!("按 {order} 的顺序创建普通索引。") },
    }];
    if unique {
        risks.push(Risk {
            level: "warn".to_owned(),
            text: "已有重复值时创建会失败。不会删除或修改任何数据。".to_owned(),
        });
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    match create_index_sql(snapshot, name.trim(), unique, columns) {
        Ok(sql) => finish(
            snapshot.engine,
            false,
            target,
            Vec::new(),
            risks,
            vec![Step { label: format!("创建索引 {name}"), sql }],
        ),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

fn plan_drop_index(snapshot: &Snapshot, table: &str, name: &str) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, table);
    if let Some(message) = guard_table(snapshot, table) {
        return finish(snapshot.engine, false, target, vec![message], Vec::new(), Vec::new());
    }
    let Some(index) = snapshot.indexes.iter().find(|index| index.name == name) else {
        return finish(snapshot.engine, false, target, vec![format!("找不到索引 {name}")], Vec::new(), Vec::new());
    };
    if index.protected {
        let reason = if index.reason.is_empty() {
            format!("索引 {name} 由约束维护，不能当作普通索引单独删除。")
        } else {
            format!("索引 {name} 不能单独删除：{}", index.reason)
        };
        return finish(snapshot.engine, false, target, vec![reason], Vec::new(), Vec::new());
    }
    let risks = vec![Risk { level: "info".to_owned(), text: "只删除这个索引，不删除表或记录。".to_owned() }];
    match drop_index_sql(snapshot, name) {
        Ok(sql) => finish(snapshot.engine, false, target, Vec::new(), risks, vec![Step { label: format!("删除索引 {name}"), sql }]),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

fn plan_create_fk(snapshot: &Snapshot, table: &str, foreign_key: &ForeignKeyDraft) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, table);
    if let Some(message) = guard_table(snapshot, table) {
        return finish(snapshot.engine, false, target, vec![message], Vec::new(), Vec::new());
    }
    let mut blocked = validate_fk(snapshot, foreign_key);
    if snapshot.storage.as_deref().is_some_and(|storage| storage.eq_ignore_ascii_case("MyISAM")) {
        blocked.push(format!("表 {table} 使用 MyISAM，不支持外键。不会通过关闭检查来假装创建成功。"));
    }
    let mut risks = vec![Risk {
        level: "info".to_owned(),
        text: "数据库会检查已有数据。存在无法匹配的值时创建失败。不会关闭约束检查，也不会清理数据。".to_owned(),
    }];
    if foreign_key.on_delete.eq_ignore_ascii_case("CASCADE") || foreign_key.on_update.eq_ignore_ascii_case("CASCADE") {
        risks.push(Risk {
            level: "warn".to_owned(),
            text: "CASCADE 会在父表变更时连带修改或删除本表中引用它的行。".to_owned(),
        });
    }
    push_fk_status(snapshot, &mut risks);
    if snapshot.engine == Engine::Sqlite {
        risks.insert(0, Risk {
            level: "warn".to_owned(),
            text: "SQLite 不能直接 ALTER TABLE 添加外键，需要按官方步骤重建表。".to_owned(),
        });
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    if snapshot.engine == Engine::Sqlite {
        let mut keys = snapshot.foreign_keys.clone();
        keys.push(fk_from_draft(foreign_key));
        return sqlite_rebuild_with(snapshot, &identity_drafts(snapshot), &keys, target, risks);
    }
    match add_fk_sql(snapshot, table, foreign_key) {
        Ok(sql) => finish(snapshot.engine, false, target, Vec::new(), risks, vec![Step { label: format!("创建外键 {}", foreign_key.name), sql }]),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

fn plan_drop_fk(snapshot: &Snapshot, table: &str, name: &str) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, table);
    if let Some(message) = guard_table(snapshot, table) {
        return finish(snapshot.engine, false, target, vec![message], Vec::new(), Vec::new());
    }
    if !snapshot.foreign_keys.iter().any(|item| item.name == name) {
        return finish(snapshot.engine, false, target, vec![format!("找不到外键 {name}")], Vec::new(), Vec::new());
    }
    let mut risks = vec![Risk {
        level: "info".to_owned(),
        text: "只删除这条外键，不删除父表、子表或记录。".to_owned(),
    }];
    push_fk_status(snapshot, &mut risks);
    if snapshot.engine == Engine::Sqlite {
        risks.insert(0, Risk { level: "warn".to_owned(), text: "SQLite 需要重建表才能移除外键。".to_owned() });
        let keys = snapshot.foreign_keys.iter().filter(|item| item.name != name).cloned().collect::<Vec<_>>();
        return sqlite_rebuild_with(snapshot, &identity_drafts(snapshot), &keys, target, risks);
    }
    let sql = match snapshot.engine {
        Engine::Postgres => format!(
            "ALTER TABLE {} DROP CONSTRAINT {};",
            qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, table).unwrap_or_else(|err| err),
            quote_ident(snapshot.engine, name)
        ),
        Engine::Mysql | Engine::Mariadb => format!(
            "ALTER TABLE {} DROP FOREIGN KEY {};",
            qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, table).unwrap_or_else(|err| err),
            quote_ident(snapshot.engine, name)
        ),
        Engine::Sqlserver => format!(
            "ALTER TABLE {} DROP CONSTRAINT {};",
            qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, table).unwrap_or_else(|err| err),
            quote_ident(snapshot.engine, name)
        ),
        Engine::Sqlite => String::new(),
        // Redis 没有结构编辑（capability 门控）；占位保持穷尽。
        Engine::Redis | Engine::Mongodb => String::new(),
    };
    if sql.contains("需要") || sql.contains("没有另开") {
        return finish(snapshot.engine, false, target, vec![sql], risks, Vec::new());
    }
    finish(snapshot.engine, false, target, Vec::new(), risks, vec![Step { label: format!("删除外键 {name}"), sql }])
}

fn plan_set_fk(snapshot: &Snapshot, enabled: bool) -> Plan {
    let target = "sqlite / foreign_keys".to_owned();
    let mut risks = vec![Risk {
        level: "info".to_owned(),
        text: "只改变本次已打开连接的 foreign_keys。不会清理已有数据，也不会把外键说成已经验证成功。".to_owned(),
    }];
    if snapshot.engine != Engine::Sqlite {
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

fn plan_alter_sequence(snapshot: &Snapshot, sequence: &str, wanted: &SequenceState) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, sequence);
    let mut risks = vec![Risk {
        level: "info".to_owned(),
        text: "序列参数改完立即生效，之后取号按新参数生成。已有数据里的号不会因此改变。".to_owned(),
    }];
    if snapshot.engine != Engine::Postgres {
        return finish(
            snapshot.engine,
            false,
            target,
            vec!["只有 PostgreSQL 的序列参数由这里修改；其它引擎请在查询里自己写 ALTER SEQUENCE。".to_owned()],
            risks,
            Vec::new(),
        );
    }
    let mut blocked = Vec::new();
    if !valid_name(sequence) {
        blocked.push("序列名无效。".to_owned());
    }
    let Some(current) = snapshot.sequence.as_ref() else {
        blocked.push(format!("没有读到序列 {sequence} 的参数，没有生成语句。"));
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    };
    if current.start.is_none() && current.increment.is_none() && current.min_value.is_none() && current.max_value.is_none() && current.cycle.is_none() {
        blocked.push(format!("序列 {sequence} 的参数读不到（可能是权限不足），没有生成语句。"));
    }
    if wanted.increment == Some(0) {
        blocked.push("步长不能是 0。".to_owned());
    }
    if let (Some(min), Some(max)) = (wanted.min_value.or(current.min_value), wanted.max_value.or(current.max_value)) {
        if min >= max {
            blocked.push("最小值必须小于最大值。".to_owned());
        }
    }
    let mut clauses: Vec<String> = Vec::new();
    if let Some(value) = wanted.increment {
        if current.increment != Some(value) {
            clauses.push(format!("INCREMENT BY {value}"));
        }
    }
    if let Some(value) = wanted.min_value {
        if current.min_value != Some(value) {
            clauses.push(format!("MINVALUE {value}"));
        }
    }
    if let Some(value) = wanted.max_value {
        if current.max_value != Some(value) {
            clauses.push(format!("MAXVALUE {value}"));
        }
    }
    if let Some(value) = wanted.start {
        if current.start != Some(value) {
            clauses.push(format!("START WITH {value}"));
        }
    }
    if let Some(cycle) = wanted.cycle {
        if current.cycle != Some(cycle) {
            clauses.push(if cycle { "CYCLE".to_owned() } else { "NO CYCLE".to_owned() });
        }
    }
    if clauses.iter().any(|clause| clause.starts_with("START WITH")) {
        risks.push(Risk {
            level: "warn".to_owned(),
            text: "START WITH 只改以后 RESTART 用的起点，不会移动已经取到的号；要移动当前号得另外 RESTART。".to_owned(),
        });
    }
    if clauses.iter().any(|clause| clause.starts_with("MAXVALUE") || clause.starts_with("MINVALUE")) {
        risks.push(Risk {
            level: "warn".to_owned(),
            text: "调小上界或调大下界可能让之后的取号失败；这里不检查已经用掉的号。".to_owned(),
        });
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    if clauses.is_empty() {
        return finish(snapshot.engine, false, target, vec![format!("序列 {sequence} 的参数没有变化。")], risks, Vec::new());
    }
    match qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, sequence) {
        Ok(name) => {
            let sql = format!("ALTER SEQUENCE {name} {};", clauses.join(" "));
            finish(snapshot.engine, false, target, Vec::new(), risks, vec![Step { label: format!("修改序列 {sequence}"), sql }])
        }
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

fn plan_add_enum_value(snapshot: &Snapshot, enum_name: &str, value: &str, before: Option<&str>) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, enum_name);
    let mut risks = vec![Risk {
        level: "info".to_owned(),
        text: "枚举值只能追加，不能删除或调整顺序；已有的行不会被改写。".to_owned(),
    }];
    if snapshot.engine != Engine::Postgres {
        return finish(snapshot.engine, false, target, vec!["只有 PostgreSQL 有枚举类型，这里不生成语句。".to_owned()], risks, Vec::new());
    }
    let trimmed = value.trim();
    let mut blocked = Vec::new();
    if !valid_name(enum_name) {
        blocked.push("枚举名无效。".to_owned());
    }
    if trimmed.is_empty() {
        blocked.push("枚举值不能为空。".to_owned());
    } else if trimmed.chars().any(char::is_control) {
        blocked.push("枚举值里不能有控制字符。".to_owned());
    } else if trimmed.len() > 63 {
        blocked.push("PostgreSQL 的枚举值最长 63 字节。".to_owned());
    }
    if snapshot.enum_values.is_empty() {
        blocked.push(format!("没有读到枚举 {enum_name} 的现有取值（可能不是枚举或权限不足），没有生成语句。"));
    } else if snapshot.enum_values.iter().any(|item| item == trimmed) {
        blocked.push(format!("{trimmed} 已经是 {enum_name} 的值，没有生成语句。"));
    }
    let before = before.map(str::trim).filter(|item| !item.is_empty());
    if let Some(at) = before {
        if !snapshot.enum_values.iter().any(|item| item == at) {
            blocked.push(format!("找不到枚举值 {at}，没有生成语句。"));
        }
        if at == trimmed {
            blocked.push("不能把新值放在它自己前面。".to_owned());
        }
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    risks.push(Risk {
        level: "warn".to_owned(),
        text: "新值要在这条语句执行完之后才能被使用；这里单条执行，不把追加值和使用它的语句放进同一个事务。".to_owned(),
    });
    let literal = format!("'{}'", trimmed.replace('\'', "''"));
    let placement = match before {
        Some(at) => format!(" BEFORE '{}'", at.replace('\'', "''")),
        None => String::new(),
    };
    match qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, enum_name) {
        Ok(name) => {
            let sql = format!("ALTER TYPE {name} ADD VALUE {literal}{placement};");
            finish(snapshot.engine, false, target, Vec::new(), risks, vec![Step { label: format!("给 {enum_name} 追加值 {trimmed}"), sql }])
        }
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

fn plan_alter_domain(snapshot: &Snapshot, domain: &str, default_sql: Option<&str>, not_null: Option<bool>) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, domain);
    let mut risks = vec![Risk {
        level: "info".to_owned(),
        text: "域的默认值只影响之后插入的行，不改已有数据。".to_owned(),
    }];
    if snapshot.engine != Engine::Postgres {
        return finish(snapshot.engine, false, target, vec!["只有 PostgreSQL 有域（domain）对象，这里不生成语句。".to_owned()], risks, Vec::new());
    }
    let mut blocked = Vec::new();
    if !valid_name(domain) {
        blocked.push("域名无效。".to_owned());
    }
    let Some(current) = snapshot.domain.as_ref() else {
        blocked.push(format!("没有读到域 {domain} 的定义，没有生成语句。"));
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    };
    let mut changes: Vec<(String, String)> = Vec::new();
    if let Some(raw) = default_sql {
        let text = raw.trim();
        if text.is_empty() || text.eq_ignore_ascii_case("null") {
            if current.default_sql.is_some() {
                changes.push(("删除域默认值".to_owned(), "DROP DEFAULT".to_owned()));
            }
        } else if !valid_default(Engine::Postgres, text) {
            blocked.push("默认值只接受字面量、TRUE/FALSE、now() 或 CURRENT_TIMESTAMP。".to_owned());
        } else if current.default_sql.as_deref() != Some(text) {
            changes.push(("修改域默认值".to_owned(), format!("SET DEFAULT {text}")));
        }
    }
    if let Some(not_null) = not_null {
        if current.not_null != not_null {
            changes.push(("修改域 NOT NULL".to_owned(), if not_null { "SET NOT NULL".to_owned() } else { "DROP NOT NULL".to_owned() }));
            if not_null {
                risks.push(Risk {
                    level: "warn".to_owned(),
                    text: "SET NOT NULL 会先检查已有数据，存在空值时数据库会报错；这里不先改写数据。".to_owned(),
                });
            }
        }
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    if changes.is_empty() {
        return finish(snapshot.engine, false, target, vec![format!("域 {domain} 的默认值与 NOT NULL 没有变化。")], risks, Vec::new());
    }
    match qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, domain) {
        Ok(name) => {
            let steps = changes
                .into_iter()
                .map(|(label, clause)| Step { sql: format!("ALTER DOMAIN {name} {clause};"), label })
                .collect::<Vec<_>>();
            finish(snapshot.engine, false, target, Vec::new(), risks, steps)
        }
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

fn plan_set_event_enabled(snapshot: &Snapshot, event: &str, enabled: bool) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, event);
    let mut risks = vec![Risk { level: "info".to_owned(), text: "只改事件的启停状态，不改它的事件体。".to_owned() }];
    if !matches!(snapshot.engine, Engine::Mysql | Engine::Mariadb) {
        return finish(snapshot.engine, false, target, vec!["只有 MySQL 与 MariaDB 有事件（event）对象，这里不生成语句。".to_owned()], risks, Vec::new());
    }
    let mut blocked = Vec::new();
    if !valid_name(event) {
        blocked.push("事件名无效。".to_owned());
    }
    match snapshot.event_enabled {
        None => blocked.push(format!("没有读到事件 {event}，没有生成语句。")),
        Some(current) if current == enabled => {
            blocked.push(format!("事件 {event} 已经是 {}。", if enabled { "enabled" } else { "disabled" }));
        }
        Some(_) => {}
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    risks.push(Risk {
        level: "warn".to_owned(),
        text: "event_scheduler 关闭时事件仍然不会执行；这里不改调度器状态。".to_owned(),
    });
    match qualified_table(snapshot.engine, &snapshot.database, "", event) {
        Ok(name) => {
            let keyword = if enabled { "ENABLE" } else { "DISABLE" };
            let sql = format!("ALTER EVENT {name} {keyword};");
            finish(
                snapshot.engine,
                false,
                target,
                Vec::new(),
                risks,
                vec![Step { label: if enabled { format!("启用事件 {event}") } else { format!("停用事件 {event}") }, sql }],
            )
        }
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

fn plan_set_pragma(snapshot: &Snapshot, pragma: &str, value: &str) -> Plan {
    let name = pragma.trim().to_ascii_lowercase();
    let target = format!("sqlite / pragma {name}");
    if snapshot.engine != Engine::Sqlite {
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

fn plan_attach_database(snapshot: &Snapshot, path: &str, name: &str) -> Plan {
    let trimmed_name = name.trim();
    let target = format!("sqlite / attach {trimmed_name}");
    if snapshot.engine != Engine::Sqlite {
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

fn plan_vacuum(snapshot: &Snapshot) -> Plan {
    let target = "sqlite / vacuum".to_owned();
    let risks = vec![
        Risk {
            level: "warn".to_owned(),
            text: "VACUUM 会重写整个数据库文件，需要额外的临时磁盘空间；不确认执行就不会写入。".to_owned(),
        },
        Risk { level: "info".to_owned(), text: "VACUUM 不能在事务里执行，这里单条执行，不改表结构也不改数据。".to_owned() },
    ];
    if snapshot.engine != Engine::Sqlite {
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

fn plan_reindex(snapshot: &Snapshot, target_name: Option<&str>) -> Plan {
    let wanted = target_name.map(str::trim).filter(|item| !item.is_empty());
    let target = match wanted {
        Some(name) => format!("sqlite / reindex {name}"),
        None => "sqlite / reindex".to_owned(),
    };
    let risks = vec![
        Risk { level: "info".to_owned(), text: "REINDEX 只重建索引，不改表里的数据。".to_owned() },
        Risk { level: "warn".to_owned(), text: "大表重建索引期间会占用写锁，其它写入要等它结束。".to_owned() },
    ];
    if snapshot.engine != Engine::Sqlite {
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

fn plan_drop_table(snapshot: &Snapshot, table: &str) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, table);
    if snapshot.name != table {
        return finish(snapshot.engine, false, target, vec!["预览的表和当前对象不一致，没有生成语句。".to_owned()], Vec::new(), Vec::new());
    }
    if snapshot.kind != "table" {
        return finish(snapshot.engine, false, target, vec!["这不是基础表，这里不会删除它。".to_owned()], Vec::new(), Vec::new());
    }
    let rows = snapshot.row_count.map(|count| format!("约 {count} 行")).unwrap_or_else(|| "现有数据".to_owned());
    let mut risks = vec![
        Risk { level: "danger".to_owned(), text: format!("将永久删除 {target} 的结构和{rows}。这不是备份。") },
        Risk { level: "info".to_owned(), text: "只删除预览中的这一张表，不隐式级联删除其他业务对象。".to_owned() },
    ];
    if !snapshot.incoming.is_empty() {
        let names = snapshot.incoming.iter().map(|item| format!("{}.{}", item.table, item.name)).collect::<Vec<_>>().join("、");
        let enforcement = if snapshot.engine == Engine::Sqlite && snapshot.live_foreign_keys == Some(false) {
            "不会添加 CASCADE。当前 foreign_keys = OFF 时 SQLite 仍可能删除这张表，引用方会留下无效引用。"
        } else {
            "不会添加 CASCADE。被引用时数据库会拒绝删除。"
        };
        risks.push(Risk { level: "warn".to_owned(), text: format!("被这些外键引用：{names}。{enforcement}") });
    }
    push_fk_status(snapshot, &mut risks);
    match qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, table) {
        Ok(name) => {
            let sql = format!("DROP TABLE {name};");
            finish(snapshot.engine, false, target, Vec::new(), risks, vec![Step { label: format!("删除 {table}"), sql }])
        }
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

fn plan_drop_view(snapshot: &Snapshot, view: &str) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, view);
    if let Some(message) = guard_view(snapshot, view) {
        return finish(snapshot.engine, false, target, vec![message], Vec::new(), Vec::new());
    }
    let mut risks = vec![
        Risk { level: "danger".to_owned(), text: format!("将永久删除视图 {target} 的定义。引用它的查询、视图和程序会失败。") },
        Risk { level: "info".to_owned(), text: "只删除预览中的这一个视图，不删除底层表的数据，也不加 CASCADE。".to_owned() },
    ];
    if snapshot.materialized {
        risks.push(Risk { level: "warn".to_owned(), text: "这是物化视图，已经物化的数据会一起删除。".to_owned() });
    }
    let dependents: Vec<&str> = snapshot.views.iter().map(|item| item.name.as_str()).filter(|name| *name != view).collect();
    if !dependents.is_empty() {
        risks.push(Risk {
            level: "warn".to_owned(),
            text: format!("这些视图可能引用它：{}。数据库可能拒绝删除，或让它们失效。", dependents.join("、")),
        });
    }
    let keyword = if snapshot.engine == Engine::Postgres && snapshot.materialized { "DROP MATERIALIZED VIEW" } else { "DROP VIEW" };
    match qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, view) {
        Ok(name) => finish(snapshot.engine, false, target, Vec::new(), risks, vec![Step { label: format!("删除视图 {view}"), sql: format!("{keyword} {name};") }]),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

fn plan_truncate(snapshot: &Snapshot, table: &str) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, table);
    if let Some(message) = guard_table(snapshot, table) {
        return finish(snapshot.engine, false, target, vec![message], Vec::new(), Vec::new());
    }
    let rows = snapshot.row_count.map(|count| format!("约 {count} 行")).unwrap_or_else(|| "全部".to_owned());
    let mut risks = vec![
        Risk { level: "danger".to_owned(), text: format!("将永久删除 {target} 里的{rows}数据，只保留表结构。这不是备份，也不能撤销。") },
    ];
    match snapshot.engine {
        Engine::Sqlite => risks.push(Risk {
            level: "info".to_owned(),
            text: "SQLite 没有 TRUNCATE，这里用 DELETE FROM 逐行删除；AUTOINCREMENT 的计数不会重置，删除触发器会触发。".to_owned(),
        }),
        Engine::Mysql | Engine::Mariadb => risks.push(Risk {
            level: "info".to_owned(),
            text: "TRUNCATE TABLE 会立即提交并把 AUTO_INCREMENT 重置到初始值，不触发 DELETE 触发器。".to_owned(),
        }),
        Engine::Postgres => risks.push(Risk {
            level: "info".to_owned(),
            text: "TRUNCATE TABLE 不触发逐行的 DELETE 触发器，不重置关联序列，也不加 CASCADE。".to_owned(),
        }),
        Engine::Sqlserver => risks.push(Risk {
            level: "info".to_owned(),
            text: "TRUNCATE TABLE 会重置 IDENTITY 计数，不触发 DELETE 触发器。".to_owned(),
        }),
        Engine::Redis | Engine::Mongodb => {}
    }
    if !snapshot.incoming.is_empty() {
        let names = snapshot.incoming.iter().map(|item| format!("{}.{}", item.table, item.name)).collect::<Vec<_>>().join("、");
        let enforcement = if snapshot.engine == Engine::Sqlite {
            if snapshot.live_foreign_keys == Some(true) {
                "当前 foreign_keys = ON：引用方还有数据时删除可能被拒绝，或按外键规则级联。"
            } else {
                "当前 foreign_keys 未开启：删除会成功，引用方会留下无效引用。"
            }
        } else {
            "被外键引用的表，数据库可能拒绝 TRUNCATE。不会添加 CASCADE。"
        };
        risks.push(Risk { level: "warn".to_owned(), text: format!("被这些外键引用：{names}。{enforcement}") });
    }
    push_fk_status(snapshot, &mut risks);
    match qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, table) {
        Ok(name) => {
            let sql = if snapshot.engine == Engine::Sqlite { format!("DELETE FROM {name};") } else { format!("TRUNCATE TABLE {name};") };
            finish(snapshot.engine, false, target, Vec::new(), risks, vec![Step { label: format!("清空 {table}"), sql }])
        }
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

fn plan_rename(snapshot: &Snapshot, table: &str, new_name: &str) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, table);
    if let Some(message) = guard_table(snapshot, table) {
        return finish(snapshot.engine, false, target, vec![message], Vec::new(), Vec::new());
    }
    let wanted = new_name.trim();
    let mut blocked = Vec::new();
    if wanted.is_empty() {
        blocked.push("新表名不能为空。".to_owned());
    } else if !valid_name(wanted) {
        blocked.push("新表名无效。".to_owned());
    } else if wanted == table {
        blocked.push("新表名和原名相同，没有生成语句。".to_owned());
    } else if snapshot.engine == Engine::Sqlite && wanted.eq_ignore_ascii_case(table) {
        // SQLite 的名字不分大小写，只改大小写会被数据库当成重名拒绝；这不是被其它对象占用。
        blocked.push(format!("SQLite 不能只改表名的大小写（{table} → {wanted}）。可以先改成另一个临时名，再改成 {wanted}。"));
    } else {
        match snapshot.name_taken {
            Some(true) => blocked.push(format!("名字 {wanted} 已经被其它对象占用，不会覆盖它。")),
            Some(false) => {}
            None => blocked.push(format!("没有确认名字 {wanted} 是否已被占用，没有生成语句。")),
        }
    }
    let mut risks = vec![Risk {
        level: "danger".to_owned(),
        text: format!("将把 {target} 改名为 {wanted}。引用旧名的查询、视图、存储过程和应用代码可能失败。"),
    }];
    let dependents: Vec<&str> = snapshot.views.iter().map(|item| item.name.as_str()).chain(snapshot.triggers.iter().map(|item| item.name.as_str())).collect();
    if !dependents.is_empty() {
        risks.push(Risk {
            level: "warn".to_owned(),
            text: format!("这些视图或触发器引用了它：{}。数据库不一定会同步改写它们。", dependents.join("、")),
        });
    }
    if snapshot.engine == Engine::Sqlserver {
        risks.push(Risk { level: "info".to_owned(), text: "sp_rename 不会改写引用旧名的存储过程、函数和视图。".to_owned() });
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    match rename_sql(snapshot, table, wanted) {
        Ok(sql) => finish(snapshot.engine, false, target, Vec::new(), risks, vec![Step { label: format!("把 {table} 改名为 {wanted}"), sql }]),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

fn rename_sql(snapshot: &Snapshot, table: &str, new_name: &str) -> Result<String, String> {
    let engine = snapshot.engine;
    let current = qualified_table(engine, &snapshot.database, &snapshot.schema, table)?;
    match engine {
        Engine::Sqlite | Engine::Postgres => Ok(format!("ALTER TABLE {current} RENAME TO {};", quote_ident(engine, new_name))),
        Engine::Mysql | Engine::Mariadb => {
            let renamed = qualified_table(engine, &snapshot.database, &snapshot.schema, new_name)?;
            Ok(format!("RENAME TABLE {current} TO {renamed};"))
        }
        // sp_rename 的新名字是裸名：加上方括号会变成名字的一部分。
        Engine::Sqlserver => Ok(format!("EXEC sp_rename N'{}', N'{}';", current.replace('\'', "''"), new_name.replace('\'', "''"))),
        Engine::Redis => Err("Redis 没有结构编辑。".to_owned()),
        Engine::Mongodb => Err("MongoDB 没有结构编辑。".to_owned()),
    }
}

/// PostgreSQL 与 SQL Server 没有保存建表原文：按读到的列、主键、外键、索引和注释重建。
/// 返回文本与没有包含的内容清单；文本开头的注释也写明它是重建的。列类型按读到的原样输出，不做编辑时的类型校验。
pub fn rebuilt_definition(snapshot: &Snapshot) -> (String, Vec<String>) {
    let engine = snapshot.engine;
    let mut missing = vec!["CHECK 约束".to_owned(), "分区".to_owned()];
    match engine {
        Engine::Postgres => missing.extend(["存储参数与表空间".to_owned(), "权限".to_owned(), "触发器".to_owned()]),
        Engine::Sqlserver => missing.extend([
            "计算列表达式".to_owned(),
            "文件组与压缩选项".to_owned(),
            "主键的 CLUSTERED / NONCLUSTERED 选项（按 SQL Server 默认处理）".to_owned(),
            "索引选项（FILLFACTOR、IGNORE_DUP_KEY 等）".to_owned(),
            "XML 与空间索引".to_owned(),
            "权限".to_owned(),
            "触发器".to_owned(),
        ]),
        _ => {}
    }
    if snapshot.columns.iter().any(|column| column.auto && !column.generated) {
        match engine {
            Engine::Postgres => missing.push("标识列的 ALWAYS / BY DEFAULT 模式与序列参数（统一写成 BY DEFAULT）".to_owned()),
            Engine::Sqlserver => missing.push("IDENTITY 的起点与步长（统一写成 1, 1）".to_owned()),
            _ => {}
        }
    }
    if let Some(message) = &snapshot.index_error {
        missing.push(format!("索引（读取失败：{message}）"));
    }
    if let Some(message) = &snapshot.foreign_key_error {
        missing.push(format!("外键（读取失败：{message}）"));
    }
    if let Some(message) = &snapshot.comment_error {
        missing.push(format!("注释（读取失败：{message}）"));
    }
    let qualified = match engine {
        Engine::Sqlite => quote_ident(engine, &snapshot.name),
        _ => qualified_table(engine, &snapshot.database, &snapshot.schema, &snapshot.name).unwrap_or_else(|_| quote_ident(engine, &snapshot.name)),
    };
    let mut lines = Vec::new();
    for column in &snapshot.columns {
        let mut sql = format!("{} {}", quote_ident(engine, &column.name), column.type_name.trim());
        let default = column.default_sql.as_deref().map(str::trim).filter(|text| !text.is_empty());
        if column.generated {
            if let (Engine::Postgres, Some(expression)) = (engine, default) {
                sql.push_str(&format!(" GENERATED ALWAYS AS ({expression}) STORED"));
            }
        } else if column.auto {
            match engine {
                Engine::Postgres => sql.push_str(" GENERATED BY DEFAULT AS IDENTITY"),
                Engine::Sqlserver => sql.push_str(" IDENTITY(1, 1)"),
                Engine::Mysql | Engine::Mariadb => sql.push_str(" AUTO_INCREMENT"),
                _ => {}
            }
        }
        if !column.nullable {
            sql.push_str(" NOT NULL");
        }
        if !column.generated && !column.auto {
            if let Some(expression) = default {
                sql.push_str(" DEFAULT ");
                sql.push_str(expression);
            }
        }
        lines.push(sql);
    }
    let primary: Vec<String> = snapshot.columns.iter().filter(|column| column.primary).map(|column| quote_ident(engine, &column.name)).collect();
    let primary_index = snapshot.indexes.iter().find(|index| is_primary_index(index));
    if !primary.is_empty() {
        match primary_index {
            Some(index) => lines.push(format!("CONSTRAINT {} PRIMARY KEY ({})", quote_ident(engine, &index.name), primary.join(", "))),
            None => lines.push(format!("PRIMARY KEY ({})", primary.join(", "))),
        }
    }
    for key in &snapshot.foreign_keys {
        let cols = key.columns.iter().map(|name| quote_ident(engine, name)).collect::<Vec<_>>().join(", ");
        let refs = key.ref_columns.iter().map(|name| quote_ident(engine, name)).collect::<Vec<_>>().join(", ");
        let ref_name = if !key.ref_schema.is_empty() {
            format!("{}.{}", quote_ident(engine, &key.ref_schema), quote_ident(engine, &key.ref_table))
        } else if engine == Engine::Sqlserver && !snapshot.schema.is_empty() {
            format!("{}.{}", quote_ident(engine, &snapshot.schema), quote_ident(engine, &key.ref_table))
        } else {
            quote_ident(engine, &key.ref_table)
        };
        lines.push(format!(
            "CONSTRAINT {} FOREIGN KEY ({cols}) REFERENCES {ref_name} ({refs}) ON DELETE {} ON UPDATE {}",
            quote_ident(engine, &key.name),
            key.on_delete,
            key.on_update
        ));
    }
    let mut text = format!(
        "-- 由元数据重建，不是数据库保存的原文。未包含：{}。\nCREATE TABLE {qualified} (\n  {}\n);",
        missing.join("、"),
        lines.join(",\n  ")
    );
    for index in &snapshot.indexes {
        if is_primary_index(index) {
            continue;
        }
        if let Some(sql) = index_definition(snapshot, index, &qualified) {
            text.push('\n');
            text.push_str(&sql);
        }
    }
    let comments = comment_statements(snapshot, &qualified);
    if !comments.is_empty() {
        text.push('\n');
        text.push_str(&comments.join("\n"));
    }
    (text, missing)
}

fn is_primary_index(index: &Index) -> bool {
    index.protected && index.reason == "由主键约束维护"
}

fn index_definition(snapshot: &Snapshot, index: &Index, qualified: &str) -> Option<String> {
    let engine = snapshot.engine;
    if let Some(sql) = index.sql.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        // 以注释开头的是读取时就说明了无法重建的索引，原样保留，不补分号。
        if sql.starts_with("--") {
            return Some(sql.to_owned());
        }
        return Some(ensure_semicolon(sql));
    }
    if index.columns.is_empty() {
        return None;
    }
    let cols = index
        .columns
        .iter()
        .map(|column| format!("{}{}", quote_ident(engine, &column.name), if column.descending { " DESC" } else { "" }))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("CREATE {}INDEX {} ON {qualified} ({cols});", if index.unique { "UNIQUE " } else { "" }, quote_ident(engine, &index.name)))
}

fn comment_statements(snapshot: &Snapshot, qualified: &str) -> Vec<String> {
    let engine = snapshot.engine;
    let literal = |text: &str| text.replace('\'', "''");
    let mut statements = Vec::new();
    match engine {
        Engine::Postgres => {
            if let Some(comment) = &snapshot.table_comment {
                statements.push(format!("COMMENT ON TABLE {qualified} IS '{}';", literal(comment)));
            }
            for column in &snapshot.columns {
                if let Some(comment) = snapshot.column_comments.get(&column.name) {
                    statements.push(format!("COMMENT ON COLUMN {qualified}.{} IS '{}';", quote_ident(engine, &column.name), literal(comment)));
                }
            }
        }
        Engine::Sqlserver => {
            let schema = literal(&snapshot.schema);
            let table = literal(&snapshot.name);
            if let Some(comment) = &snapshot.table_comment {
                statements.push(format!(
                    "EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'{}', @level0type = N'SCHEMA', @level0name = N'{schema}', @level1type = N'TABLE', @level1name = N'{table}';",
                    literal(comment)
                ));
            }
            for column in &snapshot.columns {
                if let Some(comment) = snapshot.column_comments.get(&column.name) {
                    statements.push(format!(
                        "EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'{}', @level0type = N'SCHEMA', @level0name = N'{schema}', @level1type = N'TABLE', @level1name = N'{table}', @level2type = N'COLUMN', @level2name = N'{}';",
                        literal(comment),
                        literal(&column.name)
                    ));
                }
            }
        }
        _ => {}
    }
    statements
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
    let create = match create_statement(Engine::Sqlite, &snapshot.database, &snapshot.schema, &temp, drafts, foreign_keys, snapshot.autoincrement, snapshot.without_rowid, snapshot.strict) {
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
    let restore = match snapshot.live_foreign_keys {
        Some(true) => "ON",
        Some(false) => "OFF",
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
    let mut steps = vec![
        Step { sql: "PRAGMA foreign_keys = OFF;".to_owned(), label: "在事务外关闭外键检查".to_owned() },
        Step { sql: "BEGIN;".to_owned(), label: "开始重建事务".to_owned() },
        Step { sql: create, label: "按新定义创建新表".to_owned() },
        Step {
            sql: format!("INSERT INTO {qualified_temp} ({insert_cols}) SELECT {select_cols} FROM {qualified_old};"),
            label: copy_label,
        },
        Step { sql: format!("DROP TABLE {qualified_old};"), label: "删除旧表".to_owned() },
        Step { sql: format!("ALTER TABLE {qualified_temp} RENAME TO {qualified_old};"), label: "新表改回原名".to_owned() },
    ];
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
    finish(snapshot.engine, true, target, Vec::new(), risks, steps)
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
                if !draft.nullable && !non_null_default(&draft.default_sql) {
                    return SqlitePath::Rebuild;
                }
                let qualified = match qualified_table(Engine::Sqlite, &snapshot.database, &snapshot.schema, table) {
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
                let qualified = match qualified_table(Engine::Sqlite, &snapshot.database, &snapshot.schema, table) {
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
                let default_change = normalize_default(&draft.default_sql) != original.default_sql.as_deref().map(str::trim);
                if type_change || default_change {
                    return SqlitePath::Rebuild;
                }
                let qualified = match qualified_table(Engine::Sqlite, &snapshot.database, &snapshot.schema, table) {
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

fn remote_alters(snapshot: &Snapshot, table: &str, changes: &[Change]) -> Result<Vec<Step>, String> {
    let qualified = qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, table)?;
    let mut steps = Vec::new();
    for change in changes {
        match change {
            Change::Add(draft) => {
                let definition = column_sql(snapshot.engine, draft, false)?;
                // T-SQL 没有 ADD COLUMN 关键字，只有 ADD。
                let verb = if snapshot.engine == Engine::Sqlserver { "ADD" } else { "ADD COLUMN" };
                steps.push(Step {
                    sql: format!("ALTER TABLE {qualified} {verb} {definition};"),
                    label: format!("新增字段 {}", draft.name.trim()),
                });
            }
            Change::Drop(column) => {
                steps.push(Step {
                    sql: format!("ALTER TABLE {qualified} DROP COLUMN {};", quote_ident(snapshot.engine, &column.name)),
                    label: format!("删除字段 {}", column.name),
                });
            }
            Change::Modify { original, draft } => {
                if snapshot.engine == Engine::Postgres {
                    if draft.name.trim() != original.name {
                        steps.push(Step {
                            sql: format!(
                                "ALTER TABLE {qualified} RENAME COLUMN {} TO {};",
                                quote_ident(snapshot.engine, &original.name),
                                quote_ident(snapshot.engine, draft.name.trim())
                            ),
                            label: format!("重命名 {} → {}", original.name, draft.name.trim()),
                        });
                    }
                    let column_name = quote_ident(snapshot.engine, draft.name.trim());
                    if !same_type(&draft.type_name, &original.type_name) {
                        let type_name = draft.type_name.trim();
                        steps.push(Step {
                            sql: format!("ALTER TABLE {qualified} ALTER COLUMN {column_name} TYPE {type_name} USING {column_name}::{type_name};"),
                            label: format!("修改类型 {}", draft.name.trim()),
                        });
                    }
                    if draft.nullable != original.nullable {
                        let verb = if draft.nullable { "DROP NOT NULL" } else { "SET NOT NULL" };
                        steps.push(Step {
                            sql: format!("ALTER TABLE {qualified} ALTER COLUMN {column_name} {verb};"),
                            label: if draft.nullable { format!("允许 NULL {}", draft.name.trim()) } else { format!("设置 NOT NULL {}", draft.name.trim()) },
                        });
                    }
                    if normalize_default(&draft.default_sql) != original.default_sql.as_deref().map(str::trim) {
                        let sql = if draft.default_sql.trim().is_empty() {
                            format!("ALTER TABLE {qualified} ALTER COLUMN {column_name} DROP DEFAULT;")
                        } else {
                            format!("ALTER TABLE {qualified} ALTER COLUMN {column_name} SET DEFAULT {};", draft.default_sql.trim())
                        };
                        steps.push(Step { sql, label: format!("修改默认值 {}", draft.name.trim()) });
                    }
                } else if snapshot.engine == Engine::Sqlserver {
                    // T-SQL：重命名走 sp_rename；类型/可空性走 ALTER COLUMN（不含内联 DEFAULT）；
                    // 默认值是具名约束，走 DROP CONSTRAINT / ADD CONSTRAINT ... DEFAULT ... FOR。
                    if draft.name.trim() != original.name {
                        let rename_escaped = |value: &str| value.trim().replace('\'', "''");
                        steps.push(Step {
                            sql: format!(
                                "EXEC sp_rename N'{}.{}.{}', N'{}', 'COLUMN';",
                                rename_escaped(&snapshot.schema),
                                rename_escaped(table),
                                rename_escaped(&original.name),
                                rename_escaped(draft.name.trim())
                            ),
                            label: format!("重命名 {} → {}", original.name, draft.name.trim()),
                        });
                    }
                    let column_name = quote_ident(snapshot.engine, draft.name.trim());
                    let type_changed = !same_type(&draft.type_name, &original.type_name);
                    if type_changed || draft.nullable != original.nullable {
                        let mut definition = format!("{} {}", column_name, draft.type_name.trim());
                        if !draft.nullable {
                            definition.push_str(" NOT NULL");
                        }
                        steps.push(Step {
                            sql: format!("ALTER TABLE {qualified} ALTER COLUMN {definition};"),
                            label: format!("修改字段 {}", draft.name.trim()),
                        });
                    }
                    if normalize_default(&draft.default_sql) != original.default_sql.as_deref().map(str::trim) {
                        if draft.default_sql.trim().is_empty() {
                            let sql = format!(
                                "{};",
                                super::sql::sqlserver_drop_default_sql(&qualified, draft.name.trim(), original.default_constraint.as_deref().unwrap_or(""))
                            );
                            steps.push(Step { sql, label: format!("删除默认值 {}", draft.name.trim()) });
                        } else {
                            // 既有默认值要先摘除：直接 ADD 同名约束报 2714、不同名报 1781，脚本必失败。
                            // 与对比侧 tsql_default_change 同口径（有名走 DROP CONSTRAINT，无名按 sys.default_constraints 动态查找）。
                            if original.default_sql.as_deref().map(str::trim).is_some_and(|value| !value.is_empty()) {
                                let drop = format!(
                                    "{};",
                                    super::sql::sqlserver_drop_default_sql(&qualified, original.name.trim(), original.default_constraint.as_deref().unwrap_or(""))
                                );
                                steps.push(Step { sql: drop, label: format!("删除原默认值 {}", original.name) });
                            }
                            let table_name = super::sql::sqlserver_bare_table_name(&qualified);
                            let sql = format!(
                                "{};",
                                super::sql::sqlserver_add_default_sql(&qualified, &table_name, draft.name.trim(), draft.default_sql.trim())
                            );
                            steps.push(Step { sql, label: format!("修改默认值 {}", draft.name.trim()) });
                        }
                    }
                } else {
                    let definition = column_sql(snapshot.engine, draft, false)?;
                    steps.push(Step {
                        sql: format!(
                            "ALTER TABLE {qualified} CHANGE COLUMN {} {definition};",
                            quote_ident(snapshot.engine, &original.name)
                        ),
                        label: format!("修改字段 {}", original.name),
                    });
                }
            }
        }
    }
    Ok(steps)
}

enum Change<'a> {
    Add(&'a ColumnDraft),
    Drop(&'a Column),
    Modify { original: &'a Column, draft: &'a ColumnDraft },
}

fn diff_columns<'a>(snapshot: &'a Snapshot, drafts: &'a [ColumnDraft]) -> Vec<Change<'a>> {
    let mut changes = Vec::new();
    for draft in drafts {
        if draft.dropped {
            if let Some(original) = original_of(snapshot, draft) {
                changes.push(Change::Drop(original));
            }
            continue;
        }
        match original_of(snapshot, draft) {
            None => changes.push(Change::Add(draft)),
            Some(original) => {
                let changed = draft.name.trim() != original.name
                    || !same_type(&draft.type_name, &original.type_name)
                    || draft.nullable != original.nullable
                    || normalize_default(&draft.default_sql) != original.default_sql.as_deref().map(str::trim);
                if changed {
                    changes.push(Change::Modify { original, draft });
                }
            }
        }
    }
    changes
}

fn alter_precheck(snapshot: &Snapshot, drafts: &[ColumnDraft]) -> Vec<String> {
    let mut blocked = Vec::new();
    let originals: Vec<&str> = drafts.iter().filter_map(|draft| draft.original.as_deref()).collect();
    for column in &snapshot.columns {
        if !originals.iter().any(|name| *name == column.name) {
            blocked.push(format!("字段 {} 没有出现在变更里，已停止以免漏改。", column.name));
        }
    }
    for draft in drafts {
        if draft.dropped {
            let Some(original) = original_of(snapshot, draft) else {
                continue;
            };
            if original.primary {
                blocked.push(format!("不能删除主键列 {}。", original.name));
            }
            if original.generated {
                blocked.push(format!("不能删除生成列 {}。", original.name));
            }
            if snapshot.foreign_keys.iter().any(|key| key.columns.iter().any(|name| name == &original.name)) {
                blocked.push(format!("外键引用列 {}，删除它不能保留该约束。", original.name));
            }
            if snapshot.incoming.iter().any(|key| key.ref_columns.iter().any(|name| name == &original.name)) {
                blocked.push(format!("其他表的外键引用列 {}，删除它不能保留该约束。", original.name));
            }
            continue;
        }
        let Some(original) = original_of(snapshot, draft) else {
            if draft.primary {
                blocked.push(format!("不能给已有表追加主键列 {}。", draft.name.trim()));
            }
            if snapshot.engine == Engine::Sqlite && !draft.nullable && !non_null_default(&draft.default_sql) {
                blocked.push(format!("SQLite 新增非空字段 {} 必须有非 NULL 默认值。", draft.name.trim()));
            }
            if !draft.nullable && !non_null_default(&draft.default_sql) && snapshot.row_count.unwrap_or(0) > 0 {
                blocked.push(format!("新增非空字段 {} 没有默认值，已有行无法满足 NOT NULL。", draft.name.trim()));
            }
            if !draft.nullable && !non_null_default(&draft.default_sql) && snapshot.row_count.is_none() {
                blocked.push(format!("没能确认表 {} 是否已有数据，不能新增没有默认值的非空字段 {}。", snapshot.name, draft.name.trim()));
            }
            continue;
        };
        if original.generated && (draft.name.trim() != original.name || !same_type(&draft.type_name, &original.type_name) || draft.nullable != original.nullable) {
            blocked.push(format!("生成列 {} 不能这样修改。", original.name));
        }
        if original.primary && !same_type(&draft.type_name, &original.type_name) {
            blocked.push(format!("不能在这里修改主键列 {} 的类型。", original.name));
        }
        if original.primary && draft.nullable != original.nullable {
            blocked.push(format!("不能在这里修改主键列 {} 的空值约束。", original.name));
        }
        if !original.primary && draft.primary {
            blocked.push(format!("不能把 {} 改成主键。", original.name));
        }
        if !draft.nullable && original.nullable {
            match snapshot.null_counts.get(&original.name) {
                Some(0) => {}
                Some(count) => blocked.push(format!("列 {} 有 {count} 个 NULL，不能设为 NOT NULL。请先改数据，或保留允许 NULL。", original.name)),
                None => blocked.push(format!("没能完成列 {} 的 NULL 预检查，没有生成变更。", original.name)),
            }
        }
    }
    let pk_now: Vec<&str> = snapshot.columns.iter().filter(|column| column.primary).map(|column| column.name.as_str()).collect();
    let pk_draft: Vec<&str> = drafts.iter().filter(|draft| draft.primary && !draft.dropped).filter_map(|draft| draft.original.as_deref()).collect();
    if pk_now.len() != pk_draft.len() || pk_now.iter().any(|name| !pk_draft.contains(name)) {
        blocked.push("不能在这里改变主键组成。".to_owned());
    }
    blocked
}

fn alter_risks(snapshot: &Snapshot, drafts: &[ColumnDraft]) -> Vec<Risk> {
    let mut risks = Vec::new();
    for draft in drafts {
        if draft.dropped {
            if let Some(original) = original_of(snapshot, draft) {
                risks.push(Risk {
                    level: "danger".to_owned(),
                    text: format!("删除字段 {} 会永久丢掉该列的数据。", original.name),
                });
            }
            continue;
        }
        let Some(original) = original_of(snapshot, draft) else {
            continue;
        };
        if !same_type(&draft.type_name, &original.type_name) {
            risks.push(Risk {
                level: "warn".to_owned(),
                text: format!("字段 {} 的类型由 {} 改为 {}。无法转换的值会使执行失败；RillSQL 不会自动截断或清空数据。", original.name, original.type_name, draft.type_name.trim()),
            });
        }
        if draft.name.trim() != original.name {
            risks.push(Risk {
                level: "info".to_owned(),
                text: format!("重命名 {} → {}。仍使用旧列名的查询或应用需要同步修改。", original.name, draft.name.trim()),
            });
        }
    }
    risks
}

fn validate_columns(engine: Engine, columns: &[ColumnDraft], create: bool) -> Vec<String> {
    let mut blocked = Vec::new();
    let mut names = Vec::new();
    for (index, column) in columns.iter().enumerate() {
        if column.dropped {
            continue;
        }
        let name = column.name.trim();
        if !valid_name(name) {
            blocked.push(format!("第 {} 个字段的名称无效。", index + 1));
        }
        if names.iter().any(|item: &String| item.eq_ignore_ascii_case(name)) {
            blocked.push(format!("字段名 {name} 重复。"));
        }
        names.push(name.to_owned());
        if !valid_type(engine, &column.type_name) {
            blocked.push(format!("字段 {name} 的类型 {} 不能按 {} 规则生成。", column.type_name.trim(), engine.as_str()));
        }
        if !column.default_sql.trim().is_empty() && !valid_default(engine, &column.default_sql) {
            blocked.push(format!("字段 {name} 的默认值不是允许的字面量或当前时间函数。"));
        }
        if create && column.auto && !column.primary {
            blocked.push(format!("字段 {name} 只有主键才能自增。"));
        }
    }
    blocked
}

fn validate_fk(snapshot: &Snapshot, foreign_key: &ForeignKeyDraft) -> Vec<String> {
    let mut blocked = Vec::new();
    if !valid_name(&foreign_key.name) {
        blocked.push("外键名称无效。".to_owned());
    }
    if snapshot.foreign_keys.iter().any(|item| item.name.eq_ignore_ascii_case(foreign_key.name.trim())) {
        blocked.push(format!("外键 {} 已经存在，不会覆盖。", foreign_key.name));
    }
    if foreign_key.columns.is_empty() || foreign_key.columns.len() != foreign_key.ref_columns.len() {
        blocked.push("外键两侧的列数量必须一致。".to_owned());
    }
    if !fk_actions(snapshot.engine).iter().any(|action| action.eq_ignore_ascii_case(foreign_key.on_delete.trim())) {
        blocked.push(format!("ON DELETE {} 不是 {} 支持的引用动作。", foreign_key.on_delete, snapshot.engine.as_str()));
    }
    if !fk_actions(snapshot.engine).iter().any(|action| action.eq_ignore_ascii_case(foreign_key.on_update.trim())) {
        blocked.push(format!("ON UPDATE {} 不是 {} 支持的引用动作。", foreign_key.on_update, snapshot.engine.as_str()));
    }
    let mut seen = Vec::new();
    for name in &foreign_key.columns {
        if !snapshot.columns.iter().any(|column| column.name == name.as_str()) {
            blocked.push(format!("本表没有列 {name}。"));
        }
        if seen.iter().any(|item: &String| item == name) {
            blocked.push(format!("本表列 {name} 在外键里重复。"));
        }
        seen.push(name.clone());
    }
    if let Some(target) = snapshot.reference.as_ref() {
        if target.name != foreign_key.ref_table || (!foreign_key.ref_schema.is_empty() && target.schema != foreign_key.ref_schema && matches!(snapshot.engine, Engine::Postgres | Engine::Sqlserver)) {
            blocked.push("目标表和预检查读到的表不一致。".to_owned());
        }
        for (local, remote) in foreign_key.columns.iter().zip(foreign_key.ref_columns.iter()) {
            let Some(remote_column) = target.columns.iter().find(|column| column.name == remote.as_str()) else {
                blocked.push(format!("目标表没有列 {remote}。"));
                continue;
            };
            let Some(local_column) = snapshot.columns.iter().find(|column| column.name == local.as_str()) else {
                continue;
            };
            if !compatible_type(&local_column.type_name, &remote_column.type_name) {
                blocked.push(format!("类型不匹配：{local}（{}）和 {remote}（{}）。", local_column.type_name, remote_column.type_name));
            }
        }
        let wanted = foreign_key.ref_columns.clone();
        let pk_match = target.columns.iter().filter(|column| column.primary).map(|column| column.name.clone()).collect::<Vec<_>>() == wanted
            || same_set(&target.columns.iter().filter(|column| column.primary).map(|column| column.name.as_str()).collect::<Vec<_>>(), &wanted.iter().map(String::as_str).collect::<Vec<_>>());
        let unique_match = target.indexes.iter().any(|index| {
            index.unique && !index.partial && !index.expression && same_set(&index.columns.iter().map(|column| column.name.as_str()).collect::<Vec<_>>(), &wanted.iter().map(String::as_str).collect::<Vec<_>>())
        });
        let column_unique = wanted.len() == 1 && target.columns.iter().any(|column| column.name == wanted[0] && (column.primary || column_marked_unique(target, &column.name)));
        if !pk_match && !unique_match && !column_unique {
            blocked.push("目标列必须构成目标表的主键或唯一约束。".to_owned());
        }
    } else if snapshot.engine != Engine::Sqlite || !foreign_key.ref_table.is_empty() {
        blocked.push("没有读到目标表，不能确认外键是否有效。".to_owned());
    }
    blocked
}

fn column_marked_unique(snapshot: &Snapshot, name: &str) -> bool {
    snapshot.indexes.iter().any(|index| index.unique && !index.partial && index.columns.len() == 1 && index.columns[0].name == name)
}

fn same_set(left: &[&str], right: &[&str]) -> bool {
    left.len() == right.len() && left.iter().all(|item| right.contains(item))
}

fn compatible_type(left: &str, right: &str) -> bool {
    fn base(value: &str) -> String {
        value.split('(').next().unwrap_or("").trim().to_ascii_lowercase()
    }
    let left = base(left);
    let right = base(right);
    if left == right {
        return true;
    }
    const INTS: &[&str] = &["int", "integer", "bigint", "smallint", "tinyint", "mediumint", "int2", "int4", "int8", "serial", "bigserial"];
    INTS.contains(&left.as_str()) && INTS.contains(&right.as_str())
}

#[allow(clippy::too_many_arguments)]
fn create_statement(
    engine: Engine,
    database: &str,
    schema: &str,
    table: &str,
    drafts: &[ColumnDraft],
    foreign_keys: &[ForeignKey],
    autoincrement: bool,
    without_rowid: bool,
    strict: bool,
) -> Result<String, String> {
    let qualified = if engine == Engine::Sqlite {
        quote_ident(engine, table)
    } else {
        qualified_table(engine, database, schema, table)?
    };
    let kept: Vec<&ColumnDraft> = drafts.iter().filter(|draft| !draft.dropped).collect();
    let pk: Vec<&ColumnDraft> = kept.iter().copied().filter(|draft| draft.primary).collect();
    let single_auto = engine == Engine::Sqlite && pk.len() == 1 && (pk[0].auto || autoincrement) && pk[0].type_name.to_ascii_lowercase().contains("int");
    let mut lines = Vec::new();
    for draft in &kept {
        let mut sql = column_sql(engine, draft, single_auto && draft.primary)?;
        if engine == Engine::Sqlite && single_auto && draft.primary && !sql.contains("AUTOINCREMENT") {
            sql.push_str(" PRIMARY KEY AUTOINCREMENT");
        }
        lines.push(sql);
    }
    if !pk.is_empty() && !single_auto {
        let names = pk.iter().map(|draft| quote_ident(engine, draft.name.trim())).collect::<Vec<_>>().join(", ");
        lines.push(format!("PRIMARY KEY ({names})"));
    }
    for key in foreign_keys {
        let cols = key.columns.iter().map(|name| quote_ident(engine, name)).collect::<Vec<_>>().join(", ");
        let refs = key.ref_columns.iter().map(|name| quote_ident(engine, name)).collect::<Vec<_>>().join(", ");
        let ref_name = if matches!(engine, Engine::Postgres | Engine::Sqlserver) && !key.ref_schema.is_empty() {
            format!("{}.{}", quote_ident(engine, &key.ref_schema), quote_ident(engine, &key.ref_table))
        } else {
            quote_ident(engine, &key.ref_table)
        };
        lines.push(format!(
            "CONSTRAINT {} FOREIGN KEY ({cols}) REFERENCES {ref_name} ({refs}) ON DELETE {} ON UPDATE {}",
            quote_ident(engine, &key.name),
            key.on_delete,
            key.on_update
        ));
    }
    if lines.is_empty() {
        return Err("至少需要一个字段。".to_owned());
    }
    let mut suffix = String::new();
    if engine == Engine::Sqlite {
        if without_rowid {
            suffix.push_str(" WITHOUT ROWID");
        }
        if strict {
            suffix.push_str(if without_rowid { ", STRICT" } else { " STRICT" });
        }
    }
    if matches!(engine, Engine::Mysql | Engine::Mariadb) {
        suffix.push_str(" ENGINE=InnoDB DEFAULT CHARSET=utf8mb4");
    }
    Ok(format!("CREATE TABLE {qualified} (\n  {}\n){suffix};", lines.join(",\n  ")))
}

fn column_sql(engine: Engine, draft: &ColumnDraft, sqlite_inline_auto: bool) -> Result<String, String> {
    if !valid_type(engine, &draft.type_name) {
        return Err(format!("类型 {} 无效。", draft.type_name.trim()));
    }
    if !draft.default_sql.trim().is_empty() && !valid_default(engine, &draft.default_sql) {
        return Err(format!("默认值 {} 无效。", draft.default_sql.trim()));
    }
    let mut sql = format!("{} {}", quote_ident(engine, draft.name.trim()), draft.type_name.trim());
    if engine == Engine::Postgres && draft.auto && draft.primary {
        sql.push_str(" GENERATED ALWAYS AS IDENTITY");
    }
    if !draft.nullable && !sqlite_inline_auto {
        sql.push_str(" NOT NULL");
    }
    if matches!(engine, Engine::Mysql | Engine::Mariadb) && draft.auto {
        sql.push_str(" AUTO_INCREMENT");
    }
    // T-SQL 的自增列是 IDENTITY(1, 1)。
    if engine == Engine::Sqlserver && draft.auto {
        sql.push_str(" IDENTITY(1, 1)");
    }
    if non_null_default(&draft.default_sql) && !draft.auto {
        sql.push_str(" DEFAULT ");
        sql.push_str(draft.default_sql.trim());
    }
    Ok(sql)
}

fn create_index_sql(snapshot: &Snapshot, name: &str, unique: bool, columns: &[super::model::IndexColumnDraft]) -> Result<String, String> {
    let qualified = qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, &snapshot.name)?;
    let cols = columns
        .iter()
        .map(|column| format!("{} {}", quote_ident(snapshot.engine, &column.name), if column.descending { "DESC" } else { "ASC" }))
        .collect::<Vec<_>>()
        .join(", ");
    let unique = if unique { "UNIQUE " } else { "" };
    let index = quote_ident(snapshot.engine, name);
    // PostgreSQL 的索引名不能带 schema，索引建在表所在 schema。
    Ok(format!("CREATE {unique}INDEX {index} ON {qualified} ({cols});"))
}

fn drop_index_sql(snapshot: &Snapshot, name: &str) -> Result<String, String> {
    let index = quote_ident(snapshot.engine, name);
    Ok(match snapshot.engine {
        Engine::Postgres => format!("DROP INDEX {}.{index};", quote_ident(snapshot.engine, &snapshot.schema)),
        Engine::Sqlite => format!("DROP INDEX {index};"),
        Engine::Mysql | Engine::Mariadb => format!("DROP INDEX {index} ON {};", qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, &snapshot.name)?),
        // T-SQL 的索引名不带 schema 前缀，DROP INDEX 直接落在表上。
        Engine::Sqlserver => format!("DROP INDEX {index} ON {};", qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, &snapshot.name)?),
        // Redis 没有结构编辑（capability 门控）。
        Engine::Redis => return Err("Redis 没有结构编辑。".to_owned()),
        Engine::Mongodb => return Err("MongoDB 没有结构编辑。".to_owned()),
    })
}

fn add_fk_sql(snapshot: &Snapshot, table: &str, foreign_key: &ForeignKeyDraft) -> Result<String, String> {
    let qualified = qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, table)?;
    let cols = foreign_key.columns.iter().map(|name| quote_ident(snapshot.engine, name)).collect::<Vec<_>>().join(", ");
    let refs = foreign_key.ref_columns.iter().map(|name| quote_ident(snapshot.engine, name)).collect::<Vec<_>>().join(", ");
    let ref_table = if snapshot.engine == Engine::Postgres || (snapshot.engine == Engine::Sqlserver && !foreign_key.ref_schema.is_empty()) {
        qualified_table(snapshot.engine, &snapshot.database, &foreign_key.ref_schema, &foreign_key.ref_table)?
    } else if matches!(snapshot.engine, Engine::Mysql | Engine::Mariadb) {
        qualified_table(snapshot.engine, &snapshot.database, "", &foreign_key.ref_table)?
    } else {
        quote_ident(snapshot.engine, &foreign_key.ref_table)
    };
    Ok(format!(
        "ALTER TABLE {qualified} ADD CONSTRAINT {} FOREIGN KEY ({cols}) REFERENCES {ref_table} ({refs}) ON DELETE {} ON UPDATE {};",
        quote_ident(snapshot.engine, foreign_key.name.trim()),
        foreign_key.on_delete.trim(),
        foreign_key.on_update.trim()
    ))
}

fn recreate_index_sql(snapshot: &Snapshot, index: &Index, drafts: &[ColumnDraft]) -> Result<String, String> {
    if (index.partial || index.expression) && !renamed_any(drafts) {
        let Some(sql) = index.sql.as_ref() else {
            return Err(format!("读不到索引 {} 的原始语句。", index.name));
        };
        let statement = sql.trim().trim_end_matches(';').to_owned();
        return Ok(format!("{statement};"));
    }
    let columns = index
        .columns
        .iter()
        .map(|column| {
            let name = drafts
                .iter()
                .find(|draft| draft.original.as_deref() == Some(column.name.as_str()) && !draft.dropped)
                .map(|draft| draft.name.trim().to_owned())
                .unwrap_or_else(|| column.name.clone());
            super::model::IndexColumnDraft { name, descending: column.descending }
        })
        .collect::<Vec<_>>();
    create_index_sql(snapshot, &index.name, index.unique, &columns)
}

fn preservable_indexes(snapshot: &Snapshot, drafts: &[ColumnDraft]) -> Vec<Index> {
    snapshot
        .indexes
        .iter()
        .filter(|index| {
            if index.columns.iter().any(|column| snapshot.columns.iter().any(|item| item.name == column.name && item.primary)) && index.protected {
                return false;
            }
            let dropped = index.columns.iter().any(|column| drafts.iter().any(|draft| draft.dropped && draft.original.as_deref() == Some(column.name.as_str())));
            if dropped {
                return false;
            }
            if index.columns.is_empty() && (index.partial || index.expression) {
                return index.sql.is_some() && !renamed_any(drafts);
            }
            index.columns.iter().all(|column| drafts.iter().any(|draft| draft.original.as_deref() == Some(column.name.as_str()) && !draft.dropped))
        })
        .cloned()
        .collect()
}

fn index_touches_change(index: &Index, snapshot: &Snapshot, drafts: &[ColumnDraft]) -> bool {
    index.columns.iter().any(|column| {
        drafts.iter().any(|draft| draft.original.as_deref() == Some(column.name.as_str()) && (draft.dropped || draft.name.trim() != column.name))
            || snapshot.columns.iter().any(|item| item.name == column.name)
                && drafts.iter().any(|draft| draft.dropped && draft.original.as_deref() == Some(column.name.as_str()))
    }) || ((index.partial || index.expression) && renamed_any(drafts))
}

fn column_referenced(snapshot: &Snapshot, name: &str) -> bool {
    snapshot.indexes.iter().any(|index| index.columns.iter().any(|column| column.name == name))
        || snapshot.foreign_keys.iter().any(|key| key.columns.iter().any(|column| column == name))
        || snapshot.incoming.iter().any(|key| key.ref_columns.iter().any(|column| column == name))
        || snapshot.triggers.iter().any(|trigger| mentions(&trigger.sql, name))
        || snapshot.views.iter().any(|view| mentions(&view.sql, name))
        || snapshot.columns.iter().any(|column| column.name == name && column.primary)
}

fn changed_names(snapshot: &Snapshot, drafts: &[ColumnDraft]) -> Vec<String> {
    drafts
        .iter()
        .filter_map(|draft| {
            let original = original_of(snapshot, draft)?;
            let changed = draft.dropped || draft.name.trim() != original.name || !same_type(&draft.type_name, &original.type_name);
            changed.then(|| original.name.clone())
        })
        .collect()
}

fn drafts_change_shape(snapshot: &Snapshot, drafts: &[ColumnDraft]) -> bool {
    drafts.iter().any(|draft| {
        original_of(snapshot, draft).is_none_or(|original| {
            draft.dropped || draft.name.trim() != original.name || !same_type(&draft.type_name, &original.type_name)
        })
    })
}

fn renamed_any(drafts: &[ColumnDraft]) -> bool {
    drafts.iter().any(|draft| draft.original.as_ref().is_some_and(|original| !draft.dropped && draft.name.trim() != original))
}

fn identity_drafts(snapshot: &Snapshot) -> Vec<ColumnDraft> {
    snapshot
        .columns
        .iter()
        .map(|column| ColumnDraft {
            original: Some(column.name.clone()),
            name: column.name.clone(),
            type_name: column.type_name.clone(),
            nullable: column.nullable,
            default_sql: column.default_sql.clone().unwrap_or_default(),
            primary: column.primary,
            auto: column.auto,
            dropped: false,
        })
        .collect()
}

fn fk_from_draft(draft: &ForeignKeyDraft) -> ForeignKey {
    ForeignKey {
        name: draft.name.trim().to_owned(),
        columns: draft.columns.clone(),
        ref_schema: draft.ref_schema.clone(),
        ref_table: draft.ref_table.clone(),
        ref_columns: draft.ref_columns.clone(),
        on_delete: draft.on_delete.trim().to_ascii_uppercase(),
        on_update: draft.on_update.trim().to_ascii_uppercase(),
    }
}

fn original_of<'a>(snapshot: &'a Snapshot, draft: &ColumnDraft) -> Option<&'a Column> {
    let name = draft.original.as_deref()?;
    snapshot.columns.iter().find(|column| column.name == name)
}

fn same_type(left: &str, right: &str) -> bool {
    left.trim().eq_ignore_ascii_case(right.trim())
}

fn normalize_default(value: &str) -> Option<&str> {
    let text = value.trim();
    if text.is_empty() { None } else { Some(text) }
}

fn plan_replace_view(snapshot: &Snapshot, table: &str, definition: &str) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, table);
    let mut blocked = Vec::new();
    if snapshot.kind == "missing" || !snapshot.object_exists {
        blocked.push(format!("视图 {table} 已失效，没有发送。"));
    }
    if snapshot.name != table {
        blocked.push("目标视图和已读取的结构不一致，没有生成语句。".to_owned());
    }
    if snapshot.kind != "view" && snapshot.kind != "missing" {
        blocked.push("只有视图可以替换定义。函数、存储过程和触发器不能从这里保存。".to_owned());
    }
    if let Some(message) = snapshot.definition_error.clone() {
        if snapshot.table_sql.trim().is_empty() || snapshot.table_sql.trim().starts_with("--") {
            blocked.push(format!("没有读到可编辑的视图定义：{message}"));
        }
    }
    let statements = view_statements(definition);
    if statements.is_empty() {
        blocked.push("定义只剩空白和注释，没有发送。".to_owned());
    }
    // 只看语句开头是不是物化视图。查询正文里的 table_id、tableoid 或 “MATERIALIZED VIEW” 文本不是物化视图。
    let head = match statements.as_slice() {
        [statement] => classify_view_statement(statement),
        _ => ViewHead::Other,
    };
    if snapshot.materialized || matches!(&head, ViewHead::Materialized) {
        blocked.push("无法在不丢弃物化数据的情况下替换这个物化视图，因此不发送。".to_owned());
    }
    if snapshot.engine == Engine::Sqlite {
        let mut dependents: Vec<String> = snapshot
            .views
            .iter()
            .filter(|view| view.name != snapshot.name)
            .map(|view| view.name.clone())
            .collect();
        dependents.extend(snapshot.triggers.iter().map(|trigger| trigger.name.clone()));
        if !dependents.is_empty() {
            blocked.push(format!(
                "视图被 {} 依赖，无法在不丢弃这些依赖的情况下替换，因此不发送。",
                dependents.join("、")
            ));
        }
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, Vec::new(), Vec::new());
    }
    if snapshot.table_sql.trim() == definition.trim() {
        return finish(
            snapshot.engine,
            false,
            target,
            vec!["定义没有改动，没有发送。".to_owned()],
            Vec::new(),
            Vec::new(),
        );
    }
    if statements.len() != 1 {
        return finish(
            snapshot.engine,
            false,
            target,
            vec!["定义包含多条语句，无法安全替换，没有发送。".to_owned()],
            Vec::new(),
            Vec::new(),
        );
    }
    let statement = &statements[0];
    let qualified = match qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, table) {
        Ok(value) => value,
        Err(message) => {
            return finish(snapshot.engine, false, target, vec![message], Vec::new(), Vec::new());
        }
    };
    // 名称按标识符边界比较。子串会把 table_id 当成 TABLE，或把 FROM v_notes 当成正在替换 v_notes。
    // 命中之后改写成快照的限定名和精确名称，不把无限定名或另一种大小写原样发出去。
    let bound = match &head {
        ViewHead::Query => None,
        ViewHead::Plain(view) if targets_current_view(snapshot, table, view) => Some(bind_view_name(statement, view, &qualified)),
        ViewHead::Plain(_) => {
            return finish(
                snapshot.engine,
                false,
                target,
                vec!["这条语句创建的不是当前视图，没有发送。".to_owned()],
                Vec::new(),
                Vec::new(),
            );
        }
        ViewHead::Materialized | ViewHead::Other => {
            return finish(
                snapshot.engine,
                false,
                target,
                vec!["这段定义不能安全地替换当前视图，没有发送。".to_owned()],
                Vec::new(),
                Vec::new(),
            );
        }
    };
    let risks = vec![Risk {
        level: "info".to_owned(),
        text: "将按下面的顺序替换视图定义。取消不会发送任何语句。".to_owned(),
    }];
    let steps = match snapshot.engine {
        Engine::Sqlite => {
            let create = if let Some(sql) = bound {
                ensure_semicolon(&sql)
            } else {
                format!("CREATE VIEW {qualified} AS\n{};", statement.trim().trim_end_matches(';').trim())
            };
            vec![
                Step { label: format!("删除视图 {table}"), sql: format!("DROP VIEW {qualified};") },
                Step { label: format!("创建视图 {table}"), sql: create },
            ]
        }
        Engine::Postgres | Engine::Mysql | Engine::Mariadb => {
            let sql = if let Some(sql) = bound {
                ensure_semicolon(&ensure_or_replace(&sql))
            } else {
                format!("CREATE OR REPLACE VIEW {qualified} AS\n{};", statement.trim().trim_end_matches(';').trim())
            };
            vec![Step { label: format!("替换视图 {table}"), sql }]
        }
        // T-SQL 没有 CREATE OR REPLACE VIEW，沿用 SQLite 的先删后建。
        Engine::Sqlserver => {
            let create = if let Some(sql) = bound {
                ensure_semicolon(&sql)
            } else {
                format!("CREATE VIEW {qualified} AS
{};", statement.trim().trim_end_matches(';').trim())
            };
            vec![
                Step { label: format!("删除视图 {table}"), sql: format!("DROP VIEW {qualified};") },
                Step { label: format!("创建视图 {table}"), sql: create },
            ]
        }
        // Redis 没有结构编辑（capability 门控）；占位保持穷尽。
        Engine::Redis | Engine::Mongodb => Vec::new(),
    };
    finish(snapshot.engine, false, target, Vec::new(), risks, steps)
}

fn view_statements(input: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = input.chars().collect();
    let mut index = 0;
    let mut quote: Option<char> = None;
    while index < chars.len() {
        let ch = chars[index];
        if let Some(mark) = quote {
            current.push(ch);
            if ch == mark {
                if chars.get(index + 1) == Some(&mark) {
                    current.push(mark);
                    index += 2;
                    continue;
                }
                quote = None;
            }
            index += 1;
            continue;
        }
        if ch == '-' && chars.get(index + 1) == Some(&'-') {
            index += 2;
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            continue;
        }
        if ch == '/' && chars.get(index + 1) == Some(&'*') {
            index += 2;
            while index + 1 < chars.len() && !(chars[index] == '*' && chars[index + 1] == '/') {
                index += 1;
            }
            index = (index + 2).min(chars.len());
            continue;
        }
        if ch == '\'' || ch == '"' || ch == '`' {
            quote = Some(ch);
            current.push(ch);
            index += 1;
            continue;
        }
        if ch == ';' {
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                statements.push(trimmed.to_owned());
            }
            current.clear();
            index += 1;
            continue;
        }
        current.push(ch);
        index += 1;
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        statements.push(trimmed.to_owned());
    }
    statements
}

enum ViewHead {
    Query,
    Plain(ViewTarget),
    Materialized,
    Other,
}

struct ViewTarget {
    qualifier: Option<SqlIdent>,
    name: SqlIdent,
    /// 语句里限定名和视图名的字节范围，含中间的点。
    start: usize,
    end: usize,
}

struct SqlIdent {
    text: String,
    quoted: bool,
}

struct Tok {
    start: usize,
    end: usize,
    kind: TokKind,
}

enum TokKind {
    Word(String),
    Ident(String),
    Lit,
    Symbol(char),
}

fn classify_view_statement(sql: &str) -> ViewHead {
    parse_view_head(sql).unwrap_or(ViewHead::Other)
}

fn parse_view_head(sql: &str) -> Option<ViewHead> {
    let tokens = tokenize_sql(sql);
    let mut index = 0;
    let first = tokens.get(index)?;
    if word_is(first, "select") || word_is(first, "with") {
        return Some(ViewHead::Query);
    }
    if !word_is(first, "create") {
        return None;
    }
    index += 1;
    let mut materialized = false;
    let mut temporary = false;
    loop {
        let token = tokens.get(index)?;
        if word_is(token, "view") {
            index += 1;
            break;
        }
        if word_is(token, "materialized") {
            materialized = true;
            index += 1;
            continue;
        }
        if word_is(token, "temp") || word_is(token, "temporary") {
            temporary = true;
            index += 1;
            continue;
        }
        if word_is(token, "or") {
            index += 1;
            if !tokens.get(index).is_some_and(|item| word_is(item, "replace")) {
                return None;
            }
            index += 1;
            continue;
        }
        if word_is(token, "recursive") {
            index += 1;
            continue;
        }
        if word_is(token, "algorithm") {
            index += 1;
            if tokens.get(index).is_some_and(|item| symbol_is(item, '=')) {
                index += 1;
            }
            if !tokens.get(index).is_some_and(is_word) {
                return None;
            }
            index += 1;
            continue;
        }
        if word_is(token, "definer") {
            index += 1;
            if tokens.get(index).is_some_and(|item| symbol_is(item, '=')) {
                index += 1;
            }
            index = skip_definer(&tokens, index)?;
            continue;
        }
        if word_is(token, "sql") {
            index += 1;
            if !tokens.get(index).is_some_and(|item| word_is(item, "security")) {
                return None;
            }
            index += 1;
            let security = tokens.get(index)?;
            if !(word_is(security, "definer") || word_is(security, "invoker")) {
                return None;
            }
            index += 1;
            continue;
        }
        return None;
    }
    if materialized {
        return Some(ViewHead::Materialized);
    }
    if temporary {
        return None;
    }
    if tokens.get(index).is_some_and(|item| word_is(item, "if")) {
        if !(tokens.get(index + 1).is_some_and(|item| word_is(item, "not")) && tokens.get(index + 2).is_some_and(|item| word_is(item, "exists"))) {
            return None;
        }
        index += 3;
    }
    let (name, qualifier, start, end) = read_view_name(&tokens, index)?;
    Some(ViewHead::Plain(ViewTarget { qualifier, name, start, end }))
}

fn bind_view_name(sql: &str, target: &ViewTarget, qualified: &str) -> String {
    let mut bound = String::with_capacity(sql.len() + qualified.len());
    bound.push_str(&sql[..target.start]);
    bound.push_str(qualified);
    bound.push_str(&sql[target.end..]);
    bound
}

fn skip_definer(tokens: &[Tok], mut index: usize) -> Option<usize> {
    if tokens.get(index).is_some_and(|token| word_is(token, "current_user")) {
        index += 1;
        if tokens.get(index).is_some_and(|token| symbol_is(token, '(')) {
            index += 1;
            if !tokens.get(index).is_some_and(|token| symbol_is(token, ')')) {
                return None;
            }
            index += 1;
        }
        return Some(index);
    }
    if !tokens.get(index).is_some_and(is_definer_atom) {
        return None;
    }
    index += 1;
    if tokens.get(index).is_some_and(|token| symbol_is(token, '@')) {
        index += 1;
        if !tokens.get(index).is_some_and(is_definer_atom) {
            return None;
        }
        index += 1;
    }
    Some(index)
}

fn read_view_name(tokens: &[Tok], index: usize) -> Option<(SqlIdent, Option<SqlIdent>, usize, usize)> {
    let (first, next) = take_ident(tokens, index)?;
    let start = tokens[index].start;
    if tokens.get(next).is_some_and(|token| symbol_is(token, '.')) {
        let (second, after) = take_ident(tokens, next + 1)?;
        if tokens.get(after).is_some_and(|token| symbol_is(token, '.')) {
            return None;
        }
        return Some((second, Some(first), start, tokens[next + 1].end));
    }
    Some((first, None, start, tokens[index].end))
}

fn take_ident(tokens: &[Tok], index: usize) -> Option<(SqlIdent, usize)> {
    let ident = match tokens.get(index).map(|token| &token.kind) {
        Some(TokKind::Word(text)) => SqlIdent { text: text.clone(), quoted: false },
        Some(TokKind::Ident(text)) => SqlIdent { text: text.clone(), quoted: true },
        _ => return None,
    };
    Some((ident, index + 1))
}

fn targets_current_view(snapshot: &Snapshot, table: &str, target: &ViewTarget) -> bool {
    if !ident_matches(snapshot.engine, &target.name, table) {
        return false;
    }
    match &target.qualifier {
        None => true,
        Some(qualifier) => qualifier_matches(snapshot, qualifier),
    }
}

fn qualifier_matches(snapshot: &Snapshot, qualifier: &SqlIdent) -> bool {
    match snapshot.engine {
        Engine::Postgres | Engine::Sqlserver => ident_matches(snapshot.engine, qualifier, &snapshot.schema),
        Engine::Mysql | Engine::Mariadb => ident_matches(snapshot.engine, qualifier, &snapshot.database),
        Engine::Sqlite => ident_matches(Engine::Sqlite, qualifier, &snapshot.database) || ident_matches(Engine::Sqlite, qualifier, "main"),
        // Redis 没有结构编辑（capability 门控）。
        Engine::Redis | Engine::Mongodb => false,
    }
}

fn ident_matches(engine: Engine, ident: &SqlIdent, catalog: &str) -> bool {
    if catalog.is_empty() {
        return false;
    }
    if ident.quoted {
        return ident.text == catalog;
    }
    if engine == Engine::Postgres {
        return ident.text.to_ascii_lowercase() == catalog;
    }
    ident.text.eq_ignore_ascii_case(catalog)
}

fn word_is(token: &Tok, expected: &str) -> bool {
    matches!(&token.kind, TokKind::Word(text) if text.eq_ignore_ascii_case(expected))
}

fn symbol_is(token: &Tok, expected: char) -> bool {
    matches!(token.kind, TokKind::Symbol(ch) if ch == expected)
}

fn is_word(token: &Tok) -> bool {
    matches!(token.kind, TokKind::Word(_))
}

fn is_definer_atom(token: &Tok) -> bool {
    matches!(token.kind, TokKind::Word(_) | TokKind::Ident(_) | TokKind::Lit)
}

fn tokenize_sql(sql: &str) -> Vec<Tok> {
    let mut index = 0;
    let mut tokens = Vec::new();
    while index < sql.len() {
        let ch = sql[index..].chars().next().unwrap();
        let len = ch.len_utf8();
        if ch.is_whitespace() {
            index += len;
            continue;
        }
        if ch == '-' && sql[index + len..].starts_with('-') {
            index += len + '-'.len_utf8();
            while index < sql.len() {
                let next = sql[index..].chars().next().unwrap();
                if next == '\n' {
                    break;
                }
                index += next.len_utf8();
            }
            continue;
        }
        if ch == '/' && sql[index + len..].starts_with('*') {
            index += len + '*'.len_utf8();
            while index < sql.len() {
                let next = sql[index..].chars().next().unwrap();
                let next_len = next.len_utf8();
                if next == '*' && sql[index + next_len..].starts_with('/') {
                    index += next_len + '/'.len_utf8();
                    break;
                }
                index += next_len;
            }
            continue;
        }
        if matches!(ch, '\'' | '"' | '`' | '[') {
            let start = index;
            let (kind, next) = read_quoted(sql, index);
            tokens.push(Tok { start, end: next, kind });
            index = next;
            continue;
        }
        if is_ident_start(ch) {
            let start = index;
            index += len;
            while index < sql.len() {
                let next = sql[index..].chars().next().unwrap();
                if !is_ident_continue(next) {
                    break;
                }
                index += next.len_utf8();
            }
            tokens.push(Tok { start, end: index, kind: TokKind::Word(sql[start..index].to_owned()) });
            continue;
        }
        tokens.push(Tok { start: index, end: index + len, kind: TokKind::Symbol(ch) });
        index += len;
    }
    tokens
}

fn read_quoted(sql: &str, start: usize) -> (TokKind, usize) {
    let opener = sql[start..].chars().next().unwrap();
    let closer = if opener == '[' { ']' } else { opener };
    let mut index = start + opener.len_utf8();
    let mut text = String::new();
    while index < sql.len() {
        let ch = sql[index..].chars().next().unwrap();
        let len = ch.len_utf8();
        if ch == closer {
            let next = index + len;
            let escaped = sql[next..].starts_with(closer);
            if escaped {
                text.push(closer);
                index = next + closer.len_utf8();
                continue;
            }
            let kind = if opener == '\'' { TokKind::Lit } else { TokKind::Ident(text) };
            return (kind, next);
        }
        text.push(ch);
        index += len;
    }
    let kind = if opener == '\'' { TokKind::Lit } else { TokKind::Ident(text) };
    (kind, sql.len())
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic() || (!ch.is_ascii() && ch.is_alphabetic())
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch == '$' || ch.is_ascii_alphanumeric() || (!ch.is_ascii() && ch.is_alphanumeric())
}

fn ensure_semicolon(sql: &str) -> String {
    let trimmed = sql.trim();
    if trimmed.ends_with(';') {
        trimmed.to_owned()
    } else {
        format!("{trimmed};")
    }
}

fn ensure_or_replace(sql: &str) -> String {
    let trimmed = sql.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("create or replace") {
        return trimmed.to_owned();
    }
    if lower.starts_with("create") {
        return format!("CREATE OR REPLACE{}", &trimmed[6..]);
    }
    trimmed.to_owned()
}

fn guard_table(snapshot: &Snapshot, table: &str) -> Option<String> {
    if snapshot.kind == "missing" {
        return Some(format!("找不到表 {table}。"));
    }
    if snapshot.name != table {
        return Some("目标表和已读取的结构不一致，没有生成语句。".to_owned());
    }
    if snapshot.kind != "table" {
        return Some("这是视图。首版只查看视图，不生成结构变更。".to_owned());
    }
    None
}

fn guard_view(snapshot: &Snapshot, view: &str) -> Option<String> {
    if snapshot.kind == "missing" {
        return Some(format!("找不到视图 {view}。"));
    }
    if snapshot.name != view {
        return Some("目标视图和已读取的对象不一致，没有生成语句。".to_owned());
    }
    if snapshot.kind != "view" {
        return Some("这是基础表，不是视图，这里不会删除它。".to_owned());
    }
    None
}

fn push_fk_status(snapshot: &Snapshot, risks: &mut Vec<Risk>) {
    if snapshot.engine != Engine::Sqlite {
        return;
    }
    let live = match snapshot.live_foreign_keys {
        Some(true) => "ON",
        Some(false) => "OFF",
        None => "未知",
    };
    risks.push(Risk {
        level: if snapshot.live_foreign_keys == Some(true) { "info" } else { "warn" }.to_owned(),
        text: format!("当前连接 foreign_keys = {live}。不会为了让这次操作看起来成功而静默打开或关闭它。"),
    });
}

fn target_label(engine: Engine, database: &str, schema: &str, table: &str) -> String {
    match engine {
        Engine::Sqlite => format!("sqlite / {table}"),
        Engine::Postgres => format!("postgres / {database} / {schema}.{table}"),
        Engine::Mysql => format!("mysql / {database}.{table}"),
        Engine::Mariadb => format!("mariadb / {database}.{table}"),
        Engine::Sqlserver => format!("sqlserver / {database} / {schema}.{table}"),
        // Redis 没有结构编辑（capability 门控）；占位保持穷尽。
        Engine::Redis => format!("redis / {table}"),
        Engine::Mongodb => format!("mongodb / {table}"),
    }
}

fn finish(engine: Engine, rebuild: bool, target: String, blocked: Vec<String>, risks: Vec<Risk>, steps: Vec<Step>) -> Plan {
    let blocked_now = !blocked.is_empty();
    Plan {
        blocked,
        risks,
        steps: if blocked_now { Vec::new() } else { steps },
        rule: rule_text(engine, rebuild && !blocked_now),
        rebuild: rebuild && !blocked_now,
        target,
    }
}
