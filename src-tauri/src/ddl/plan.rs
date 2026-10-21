use super::model::{
    fk_actions, rule_text, Action, Column, ColumnDraft, Engine, ForeignKey, ForeignKeyDraft, Index, Plan, Risk, Snapshot, Step,
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
        Action::SetForeignKeys { enabled } => plan_set_fk(snapshot, *enabled),
        Action::ReplaceView { table, definition } => plan_replace_view(snapshot, table, definition),
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
        text: format!("按 {order} 的顺序创建{}索引。", if unique { "唯一" } else { "普通" }),
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
        Engine::Sqlite => String::new(),
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
        if index.expression && index.columns.is_empty() && !dropped {
            if renamed_any(drafts) {
                return Some(format!("表达式索引 {} 在列重命名后无法可靠改写。", index.name));
            }
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
                        label: format!("{} {}", if draft.nullable { "去掉 NOT NULL" } else { "设置 NOT NULL" }, draft.name.trim()),
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
                steps.push(Step {
                    sql: format!("ALTER TABLE {qualified} ADD COLUMN {definition};"),
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
                            label: format!("{} {}", if draft.nullable { "允许 NULL" } else { "设置 NOT NULL" }, draft.name.trim()),
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
                text: format!("字段 {} 的类型由 {} 改为 {}。无法转换的值会使执行失败；澄库不会自动截断或清空数据。", original.name, original.type_name, draft.type_name.trim()),
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
        if target.name != foreign_key.ref_table || (!foreign_key.ref_schema.is_empty() && target.schema != foreign_key.ref_schema && snapshot.engine == Engine::Postgres) {
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
        let ref_name = if engine == Engine::Postgres && !key.ref_schema.is_empty() {
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
    })
}

fn add_fk_sql(snapshot: &Snapshot, table: &str, foreign_key: &ForeignKeyDraft) -> Result<String, String> {
    let qualified = qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, table)?;
    let cols = foreign_key.columns.iter().map(|name| quote_ident(snapshot.engine, name)).collect::<Vec<_>>().join(", ");
    let refs = foreign_key.ref_columns.iter().map(|name| quote_ident(snapshot.engine, name)).collect::<Vec<_>>().join(", ");
    let ref_table = if snapshot.engine == Engine::Postgres {
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
    let plain = match &head {
        ViewHead::Query => None,
        ViewHead::Plain(view) if targets_current_view(snapshot, table, view) => Some(statement.as_str()),
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
            let create = if let Some(sql) = plain {
                ensure_semicolon(sql)
            } else {
                format!("CREATE VIEW {qualified} AS\n{};", statement.trim().trim_end_matches(';').trim())
            };
            vec![
                Step { label: format!("删除视图 {table}"), sql: format!("DROP VIEW {qualified};") },
                Step { label: format!("创建视图 {table}"), sql: create },
            ]
        }
        Engine::Postgres | Engine::Mysql | Engine::Mariadb => {
            let sql = if let Some(sql) = plain {
                ensure_semicolon(&ensure_or_replace(sql))
            } else {
                format!("CREATE OR REPLACE VIEW {qualified} AS\n{};", statement.trim().trim_end_matches(';').trim())
            };
            vec![Step { label: format!("替换视图 {table}"), sql }]
        }
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
}

struct SqlIdent {
    text: String,
    quoted: bool,
}

enum Tok {
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
            if matches!(tokens.get(index), Some(Tok::Symbol('='))) {
                index += 1;
            }
            if !matches!(tokens.get(index), Some(Tok::Word(_))) {
                return None;
            }
            index += 1;
            continue;
        }
        if word_is(token, "definer") {
            index += 1;
            if matches!(tokens.get(index), Some(Tok::Symbol('='))) {
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
    let (name, qualifier) = read_view_name(&tokens, index)?;
    Some(ViewHead::Plain(ViewTarget { qualifier, name }))
}

fn skip_definer(tokens: &[Tok], mut index: usize) -> Option<usize> {
    if tokens.get(index).is_some_and(|token| word_is(token, "current_user")) {
        index += 1;
        if matches!(tokens.get(index), Some(Tok::Symbol('('))) {
            index += 1;
            if !matches!(tokens.get(index), Some(Tok::Symbol(')'))) {
                return None;
            }
            index += 1;
        }
        return Some(index);
    }
    if !matches!(tokens.get(index), Some(Tok::Word(_) | Tok::Ident(_) | Tok::Lit)) {
        return None;
    }
    index += 1;
    if matches!(tokens.get(index), Some(Tok::Symbol('@'))) {
        index += 1;
        if !matches!(tokens.get(index), Some(Tok::Word(_) | Tok::Ident(_) | Tok::Lit)) {
            return None;
        }
        index += 1;
    }
    Some(index)
}

fn read_view_name(tokens: &[Tok], mut index: usize) -> Option<(SqlIdent, Option<SqlIdent>)> {
    let (first, next) = take_ident(tokens, index)?;
    index = next;
    if matches!(tokens.get(index), Some(Tok::Symbol('.'))) {
        index += 1;
        let (second, next) = take_ident(tokens, index)?;
        if matches!(tokens.get(next), Some(Tok::Symbol('.'))) {
            return None;
        }
        return Some((second, Some(first)));
    }
    Some((first, None))
}

fn take_ident(tokens: &[Tok], index: usize) -> Option<(SqlIdent, usize)> {
    let ident = match tokens.get(index)? {
        Tok::Word(text) => SqlIdent { text: text.clone(), quoted: false },
        Tok::Ident(text) => SqlIdent { text: text.clone(), quoted: true },
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
        Engine::Postgres => ident_matches(snapshot.engine, qualifier, &snapshot.schema),
        Engine::Mysql | Engine::Mariadb => ident_matches(snapshot.engine, qualifier, &snapshot.database),
        Engine::Sqlite => ident_matches(Engine::Sqlite, qualifier, &snapshot.database) || ident_matches(Engine::Sqlite, qualifier, "main"),
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
    matches!(token, Tok::Word(text) if text.eq_ignore_ascii_case(expected))
}

fn tokenize_sql(sql: &str) -> Vec<Tok> {
    let chars: Vec<char> = sql.chars().collect();
    let mut index = 0;
    let mut tokens = Vec::new();
    while index < chars.len() {
        let ch = chars[index];
        if ch.is_whitespace() {
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
        if matches!(ch, '\'' | '"' | '`' | '[') {
            let (token, next) = read_quoted(&chars, index);
            tokens.push(token);
            index = next;
            continue;
        }
        if is_ident_start(ch) {
            let start = index;
            index += 1;
            while index < chars.len() && is_ident_continue(chars[index]) {
                index += 1;
            }
            tokens.push(Tok::Word(chars[start..index].iter().collect()));
            continue;
        }
        tokens.push(Tok::Symbol(ch));
        index += 1;
    }
    tokens
}

fn read_quoted(chars: &[char], start: usize) -> (Tok, usize) {
    let opener = chars[start];
    let closer = if opener == '[' { ']' } else { opener };
    let mut index = start + 1;
    let mut text = String::new();
    while index < chars.len() {
        let ch = chars[index];
        if ch == closer {
            let escaped = if opener == '[' {
                chars.get(index + 1) == Some(&']')
            } else {
                chars.get(index + 1) == Some(&closer)
            };
            if escaped {
                text.push(closer);
                index += 2;
                continue;
            }
            index += 1;
            let token = if opener == '\'' { Tok::Lit } else { Tok::Ident(text) };
            return (token, index);
        }
        text.push(ch);
        index += 1;
    }
    let token = if opener == '\'' { Tok::Lit } else { Tok::Ident(text) };
    (token, chars.len())
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
