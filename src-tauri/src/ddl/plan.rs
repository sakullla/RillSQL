//! 结构变更计划。这里只写引擎无关的流程与校验；方言差异（语句形状、限定名、重建、库级动作）经
//! [`DdlDialect`](crate::adapters::ddl::DdlDialect) 取得。

use crate::adapters::ddl::{ddl_of, no_structure_editing, DdlDialect, ViewModifier};
use crate::adapters::adapter;

use super::model::{fk_actions, rule_text, Action, Column, ColumnDraft, Engine, ForeignKey, ForeignKeyDraft, Index, Plan, Risk, Snapshot, Step, TableOptions};
use super::sql::{default_problem, default_sql, existing_default, mentions, qualified_table, quote_ident, valid_name, valid_type};

pub fn plan(snapshot: &Snapshot, action: &Action) -> Plan {
    match action {
        Action::CreateTable { name, columns, options } => plan_create(snapshot, name, columns, options),
        Action::AlterColumns { table, columns } => plan_alter(snapshot, table, columns),
        Action::CreateIndex { table, name, unique, columns } => plan_create_index(snapshot, table, name, *unique, columns),
        Action::DropIndex { table, name } => plan_drop_index(snapshot, table, name),
        Action::CreateForeignKey { table, foreign_key } => plan_create_fk(snapshot, table, foreign_key),
        Action::DropForeignKey { table, name } => plan_drop_fk(snapshot, table, name),
        Action::DropTable { table } => plan_drop_table(snapshot, table),
        Action::DropView { view } => plan_drop_view(snapshot, view),
        Action::TruncateTable { table } => plan_truncate(snapshot, table),
        Action::RenameTable { table, new_name } => plan_rename(snapshot, table, new_name),
        Action::ReplaceView { table, definition } => plan_replace_view(snapshot, table, definition),
        // 引擎专属的库级动作：所属方言给计划；别的引擎请求时由所属方言给出拒绝原因。
        Action::SetForeignKeys { .. }
        | Action::AlterSequence { .. }
        | Action::AddEnumValue { .. }
        | Action::AlterDomain { .. }
        | Action::SetEventEnabled { .. }
        | Action::SetPragma { .. }
        | Action::AttachDatabase { .. }
        | Action::Vacuum
        | Action::Reindex { .. } => crate::adapters::ddl::plan_object_action(snapshot, action).unwrap_or_else(|| {
            let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, &snapshot.name);
            finish(snapshot.engine, false, target, vec![no_structure_editing(snapshot.engine)], Vec::new(), Vec::new())
        }),
    }
}

/// 结构编辑的方言；没有结构编辑的引擎给出原因。
fn dialect_of(engine: Engine) -> Result<&'static dyn DdlDialect, String> {
    ddl_of(engine).ok_or_else(|| no_structure_editing(engine))
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
            default: existing_default(snapshot.engine, column.default_sql.as_deref()),
            comment: String::new(),
            primary: column.primary,
            auto: column.auto,
            dropped: false,
        })
        .collect();
    let options = TableOptions { without_rowid: snapshot.without_rowid, strict: snapshot.strict, ..TableOptions::default() };
    match create_statement(snapshot.engine, &snapshot.database, &snapshot.schema, &snapshot.name, &columns, &snapshot.foreign_keys, snapshot.autoincrement, &options) {
        Ok(sql) => sql,
        Err(message) => message,
    }
}

fn plan_create(snapshot: &Snapshot, name: &str, columns: &[ColumnDraft], options: &TableOptions) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, name);
    let mut blocked = validate_columns(snapshot, columns, true);
    if !valid_name(name) {
        blocked.push("表名无效。".to_owned());
    }
    for (label, value) in [("ENGINE", &options.engine), ("CHARSET", &options.charset), ("COLLATE", &options.collation)] {
        let text = value.trim();
        if !text.is_empty() && !text.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
            blocked.push(format!("表选项 {label} 的值 {text} 无效。"));
        }
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
    match create_statement(snapshot.engine, &snapshot.database, &snapshot.schema, name.trim(), columns, &[], autoincrement, options) {
        Ok(sql) => {
            let mut steps = vec![Step { label: format!("创建 {name}"), sql }];
            steps.extend(create_comment_steps(snapshot, name.trim(), columns, options));
            finish(snapshot.engine, false, target, Vec::new(), risks, steps)
        }
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

/// 建表后另写的注释语句（不支持内联注释的方言）。内联注释的方言在这里不产生语句。
fn create_comment_steps(snapshot: &Snapshot, name: &str, columns: &[ColumnDraft], options: &TableOptions) -> Vec<Step> {
    let Some(dialect) = ddl_of(snapshot.engine) else {
        return Vec::new();
    };
    let spec = dialect.table_option_spec();
    let mut created = Snapshot::bare(snapshot.engine, &snapshot.database, &snapshot.schema, name);
    if spec.comment && !options.comment.trim().is_empty() {
        created.table_comment = Some(options.comment.trim().to_owned());
    }
    for draft in columns.iter().filter(|draft| !draft.dropped) {
        created.columns.push(Column {
            name: draft.name.trim().to_owned(),
            type_name: draft.type_name.trim().to_owned(),
            nullable: draft.nullable,
            default_sql: None,
            primary: draft.primary,
            auto: draft.auto,
            generated: false,
            default_constraint: None,
        });
        if spec.column_comments && !draft.comment.trim().is_empty() && dialect.inline_column_comment(draft.comment.trim()).is_none() {
            created.column_comments.insert(draft.name.trim().to_owned(), draft.comment.trim().to_owned());
        }
    }
    let Ok(qualified) = dialect.create_table_name(&snapshot.database, &snapshot.schema, name) else {
        return Vec::new();
    };
    dialect
        .comment_statements(&created, &qualified)
        .into_iter()
        .map(|sql| Step { label: format!("写入注释 {name}"), sql })
        .collect()
}

const NO_COLUMN_CHANGE: &str = "没有字段变更。";

/// 预览拦下的原因只是“草稿和现状一样”：界面据此用说明而不是错误的口吻。
pub(crate) fn is_no_change(text: &str) -> bool {
    text == NO_COLUMN_CHANGE
}

fn plan_alter(snapshot: &Snapshot, table: &str, drafts: &[ColumnDraft]) -> Plan {
    let target = target_label(snapshot.engine, &snapshot.database, &snapshot.schema, table);
    if let Some(message) = guard_table(snapshot, table) {
        return finish(snapshot.engine, false, target, vec![message], Vec::new(), Vec::new());
    }
    let mut blocked = validate_columns(snapshot, drafts, false);
    blocked.extend(alter_precheck(snapshot, drafts));
    let risks = alter_risks(snapshot, drafts);
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    let changes = diff_columns(snapshot, drafts);
    if changes.is_empty() {
        return finish(snapshot.engine, false, target, vec![NO_COLUMN_CHANGE.to_owned()], risks, Vec::new());
    }
    match dialect_of(snapshot.engine) {
        Ok(dialect) => dialect.alter_plan(snapshot, table, drafts, &changes, target, risks),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

/// 修改一个字段的方言回调：`(qualified, original, draft)` → 步骤。
pub(crate) type ModifyColumn<'a> = &'a dyn Fn(&str, &Column, &ColumnDraft) -> Result<Vec<Step>, String>;

/// 按语句直接改字段的计划：新增与删除走通用写法，修改由方言给出语句。`add_verb` 是新增字段的关键字。
/// `modify(qualified, original, draft)` 生成修改一个字段的全部步骤。
pub(crate) fn statement_alter_plan(
    snapshot: &Snapshot,
    table: &str,
    changes: &[Change],
    target: String,
    risks: Vec<Risk>,
    add_verb: &str,
    modify: ModifyColumn<'_>,
) -> Plan {
    match statement_alters(snapshot, table, changes, add_verb, modify) {
        Ok(steps) => finish(snapshot.engine, false, target, Vec::new(), risks, steps),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

fn statement_alters(
    snapshot: &Snapshot,
    table: &str,
    changes: &[Change],
    add_verb: &str,
    modify: ModifyColumn<'_>,
) -> Result<Vec<Step>, String> {
    let qualified = qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, table)?;
    let mut steps = Vec::new();
    for change in changes {
        match change {
            Change::Add(draft) => {
                let definition = column_sql(snapshot.engine, draft, false)?;
                steps.push(Step {
                    sql: format!("ALTER TABLE {qualified} {add_verb} {definition};"),
                    label: format!("新增字段 {}", draft.name.trim()),
                });
            }
            Change::Drop(column) => {
                steps.push(Step {
                    sql: format!("ALTER TABLE {qualified} DROP COLUMN {};", quote_ident(snapshot.engine, &column.name)),
                    label: format!("删除字段 {}", column.name),
                });
            }
            Change::Modify { original, draft } => steps.extend(modify(&qualified, original, draft)?),
        }
    }
    Ok(steps)
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
    let sql = dialect_of(snapshot.engine).and_then(|dialect| dialect.drop_index_sql(snapshot, &quote_ident(snapshot.engine, name)));
    match sql {
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
    if let Some(message) = ddl_of(snapshot.engine).and_then(|dialect| dialect.foreign_key_blocker(snapshot, table)) {
        blocked.push(message);
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
    let dialect = ddl_of(snapshot.engine);
    if let Some(text) = dialect.and_then(|dialect| dialect.foreign_key_change_risk(true)) {
        risks.insert(0, Risk { level: "warn".to_owned(), text: text.to_owned() });
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    if let Some(dialect) = dialect {
        let mut keys = snapshot.foreign_keys.clone();
        keys.push(fk_from_draft(foreign_key));
        if let Some(plan) = dialect.rebuild_with_foreign_keys(snapshot, &keys, &target, &risks) {
            return plan;
        }
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
    let dialect = match dialect_of(snapshot.engine) {
        Ok(dialect) => dialect,
        Err(message) => return finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    };
    if let Some(text) = dialect.foreign_key_change_risk(false) {
        risks.insert(0, Risk { level: "warn".to_owned(), text: text.to_owned() });
    }
    let keys = snapshot.foreign_keys.iter().filter(|item| item.name != name).cloned().collect::<Vec<_>>();
    if let Some(plan) = dialect.rebuild_with_foreign_keys(snapshot, &keys, &target, &risks) {
        return plan;
    }
    let sql = format!(
        "ALTER TABLE {} {} {};",
        qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, table).unwrap_or_else(|err| err),
        dialect.drop_foreign_key_clause(),
        quote_ident(snapshot.engine, name)
    );
    if sql.contains("需要") || sql.contains("没有另开") {
        return finish(snapshot.engine, false, target, vec![sql], risks, Vec::new());
    }
    finish(snapshot.engine, false, target, Vec::new(), risks, vec![Step { label: format!("删除外键 {name}"), sql }])
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
        let enforcement = ddl_of(snapshot.engine).map(|dialect| dialect.drop_referenced_note(snapshot.live_foreign_keys)).unwrap_or_default();
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
    let keyword = ddl_of(snapshot.engine).map(|dialect| dialect.drop_view_keyword(snapshot.materialized)).unwrap_or_default();
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
    let dialect = ddl_of(snapshot.engine);
    if let Some(dialect) = dialect {
        risks.push(Risk { level: "info".to_owned(), text: dialect.truncate_note().to_owned() });
    }
    if !snapshot.incoming.is_empty() {
        let names = snapshot.incoming.iter().map(|item| format!("{}.{}", item.table, item.name)).collect::<Vec<_>>().join("、");
        let enforcement = dialect.map(|dialect| dialect.truncate_referenced_note(snapshot.live_foreign_keys)).unwrap_or_default();
        risks.push(Risk { level: "warn".to_owned(), text: format!("被这些外键引用：{names}。{enforcement}") });
    }
    push_fk_status(snapshot, &mut risks);
    match qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, table) {
        Ok(name) => {
            let sql = dialect.map(|dialect| dialect.truncate_sql(&name)).unwrap_or_default();
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
    } else if let Some(message) = ddl_of(snapshot.engine).and_then(|dialect| dialect.rename_blocker(table, wanted)) {
        blocked.push(message);
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
    if let Some(note) = ddl_of(snapshot.engine).and_then(|dialect| dialect.rename_note()) {
        risks.push(Risk { level: "info".to_owned(), text: note.to_owned() });
    }
    if !blocked.is_empty() {
        return finish(snapshot.engine, false, target, blocked, risks, Vec::new());
    }
    match dialect_of(snapshot.engine).and_then(|dialect| dialect.rename_table_sql(snapshot, table, wanted)) {
        Ok(sql) => finish(snapshot.engine, false, target, Vec::new(), risks, vec![Step { label: format!("把 {table} 改名为 {wanted}"), sql }]),
        Err(message) => finish(snapshot.engine, false, target, vec![message], risks, Vec::new()),
    }
}

/// 没有保存建表原文的引擎：按读到的列、主键、外键、索引和注释重建。
/// 返回文本与没有包含的内容清单；文本开头的注释也写明它是重建的。列类型按读到的原样输出，不做编辑时的类型校验。
pub fn rebuilt_definition(snapshot: &Snapshot) -> (String, Vec<String>) {
    let engine = snapshot.engine;
    let dialect = ddl_of(engine);
    let mut missing = vec!["CHECK 约束".to_owned(), "分区".to_owned()];
    if let Some(dialect) = dialect {
        missing.extend(dialect.rebuilt_missing().iter().map(|item| (*item).to_owned()));
        if snapshot.columns.iter().any(|column| column.auto && !column.generated) {
            missing.extend(dialect.rebuilt_identity_missing().map(str::to_owned));
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
    let qualified = display_qualified(snapshot);
    let mut lines = Vec::new();
    for column in &snapshot.columns {
        let mut sql = format!("{} {}", quote_ident(engine, &column.name), column.type_name.trim());
        let default = column.default_sql.as_deref().map(str::trim).filter(|text| !text.is_empty());
        if column.generated {
            if let Some(clause) = default.and_then(|expression| dialect.and_then(|dialect| dialect.generated_column_clause(expression))) {
                sql.push_str(&clause);
            }
        } else if column.auto {
            sql.push_str(dialect.map(|dialect| dialect.rebuilt_auto_clause()).unwrap_or_default());
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
        let implied = dialect.map(|dialect| dialect.implied_reference_schema(&snapshot.schema)).unwrap_or_default();
        let ref_name = if !key.ref_schema.is_empty() {
            format!("{}.{}", quote_ident(engine, &key.ref_schema), quote_ident(engine, &key.ref_table))
        } else if !implied.is_empty() {
            format!("{}.{}", quote_ident(engine, implied), quote_ident(engine, &key.ref_table))
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
    let comments = dialect.map(|dialect| dialect.comment_statements(snapshot, &qualified)).unwrap_or_default();
    if !comments.is_empty() {
        text.push('\n');
        text.push_str(&comments.join("\n"));
    }
    (text, missing)
}

/// 展示用的限定表名：限定名生成不了（没有库/模式、不是当前库）时只写表名。
fn display_qualified(snapshot: &Snapshot) -> String {
    qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, &snapshot.name).unwrap_or_else(|_| quote_ident(snapshot.engine, &snapshot.name))
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

/// 供结构页复制或展示单条索引的建索引 SQL。
pub(crate) fn index_sql_for_display(snapshot: &Snapshot, index: &Index) -> Option<String> {
    let engine = snapshot.engine;
    let qualified = display_qualified(snapshot);
    if is_primary_index(index) {
        let cols = index
            .columns
            .iter()
            .map(|column| quote_ident(engine, &column.name))
            .collect::<Vec<_>>()
            .join(", ");
        if cols.is_empty() {
            return None;
        }
        return Some(format!("ALTER TABLE {qualified} ADD CONSTRAINT {} PRIMARY KEY ({cols});", quote_ident(engine, &index.name)));
    }
    index_definition(snapshot, index, &qualified)
}

/// 供结构页复制或展示单条外键的约束 SQL。
pub(crate) fn fk_sql_for_display(snapshot: &Snapshot, key: &ForeignKey) -> Option<String> {
    let draft = ForeignKeyDraft {
        name: if key.name.is_empty() { format!("fk_{}", snapshot.name) } else { key.name.clone() },
        columns: key.columns.clone(),
        ref_schema: key.ref_schema.clone(),
        ref_table: key.ref_table.clone(),
        ref_columns: key.ref_columns.clone(),
        on_delete: key.on_delete.clone(),
        on_update: key.on_update.clone(),
    };
    add_fk_sql(snapshot, &snapshot.name, &draft).ok()
}

pub(crate) enum Change<'a> {
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
                    || default_changed(snapshot.engine, original, draft);
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
            if !draft.nullable && !draft.default.has_value() {
                if let Some(message) = ddl_of(snapshot.engine).and_then(|dialect| dialect.add_not_null_without_default(draft.name.trim())) {
                    blocked.push(message);
                }
            }
            if !draft.nullable && !draft.default.has_value() && snapshot.row_count.unwrap_or(0) > 0 {
                blocked.push(format!("新增非空字段 {} 没有默认值，已有行无法满足 NOT NULL。", draft.name.trim()));
            }
            if !draft.nullable && !draft.default.has_value() && snapshot.row_count.is_none() {
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

fn validate_columns(snapshot: &Snapshot, columns: &[ColumnDraft], create: bool) -> Vec<String> {
    let engine = snapshot.engine;
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
        // 修改表时，没动过的类型和默认值是数据库自己报回来的（如 PostgreSQL 的
        // nextval('…'::regclass)、'paid'::text），不会被写进语句，也就不按手输规则拦下：
        // 否则带自增序列或带类型转换默认值的表连改一个列名都做不到。
        let original = if create { None } else { original_of(snapshot, column) };
        let type_kept = original.is_some_and(|original| same_type(&column.type_name, &original.type_name));
        let default_kept = original.is_some_and(|original| !default_changed(engine, original, column));
        if !type_kept && !valid_type(engine, &column.type_name) {
            blocked.push(format!("字段 {name} 的类型 {} 不能按 {} 规则生成。", column.type_name.trim(), engine.as_str()));
        }
        if !default_kept {
            if let Some(problem) = default_problem(engine, column) {
                blocked.push(problem);
            }
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
        let cross_schema = ddl_of(snapshot.engine).is_some_and(|dialect| dialect.references_cross_schema());
        if target.name != foreign_key.ref_table || (!foreign_key.ref_schema.is_empty() && target.schema != foreign_key.ref_schema && cross_schema) {
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
    } else if ddl_of(snapshot.engine).is_none_or(|dialect| dialect.requires_reference_read()) || !foreign_key.ref_table.is_empty() {
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
pub(crate) fn create_statement(
    engine: Engine,
    database: &str,
    schema: &str,
    table: &str,
    drafts: &[ColumnDraft],
    foreign_keys: &[ForeignKey],
    autoincrement: bool,
    options: &TableOptions,
) -> Result<String, String> {
    let dialect = dialect_of(engine)?;
    let qualified = dialect.create_table_name(database, schema, table)?;
    let kept: Vec<&ColumnDraft> = drafts.iter().filter(|draft| !draft.dropped).collect();
    let pk: Vec<&ColumnDraft> = kept.iter().copied().filter(|draft| draft.primary).collect();
    let single_auto = dialect.inline_autoincrement_primary_key()
        && pk.len() == 1
        && (pk[0].auto || autoincrement)
        && pk[0].type_name.to_ascii_lowercase().contains("int");
    let mut lines = Vec::new();
    for draft in &kept {
        let mut sql = column_sql(engine, draft, single_auto && draft.primary)?;
        if single_auto && draft.primary {
            dialect.finish_inline_autoincrement(&mut sql);
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
        let ref_name = if dialect.references_cross_schema() && !key.ref_schema.is_empty() {
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
    let suffix = dialect.create_table_suffix(options);
    Ok(format!("CREATE TABLE {qualified} (\n  {}\n){suffix};", lines.join(",\n  ")))
}

/// 字段定义。`inline_auto` 为真时这一列是内联的自增主键，不再单独写 NOT NULL。
pub(crate) fn column_sql(engine: Engine, draft: &ColumnDraft, inline_auto: bool) -> Result<String, String> {
    if !valid_type(engine, &draft.type_name) {
        return Err(format!("类型 {} 无效。", draft.type_name.trim()));
    }
    let default = default_sql(engine, draft).map_err(|_| format!("默认值 {} 无效。", draft.default.label().trim()))?;
    let dialect = dialect_of(engine)?;
    let mut sql = format!("{} {}", quote_ident(engine, draft.name.trim()), draft.type_name.trim());
    sql.push_str(dialect.auto_clause_before_null(draft));
    if !draft.nullable && !inline_auto {
        sql.push_str(" NOT NULL");
    }
    sql.push_str(dialect.auto_clause_after_null(draft));
    if let Some(default) = default.filter(|_| !draft.auto) {
        sql.push_str(" DEFAULT ");
        sql.push_str(&default);
    }
    if let Some(comment) = Some(draft.comment.trim()).filter(|text| !text.is_empty()).and_then(|text| dialect.inline_column_comment(text)) {
        sql.push_str(&comment);
    }
    Ok(sql)
}

pub(crate) fn create_index_sql(snapshot: &Snapshot, name: &str, unique: bool, columns: &[super::model::IndexColumnDraft]) -> Result<String, String> {
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

fn add_fk_sql(snapshot: &Snapshot, table: &str, foreign_key: &ForeignKeyDraft) -> Result<String, String> {
    let qualified = qualified_table(snapshot.engine, &snapshot.database, &snapshot.schema, table)?;
    let cols = foreign_key.columns.iter().map(|name| quote_ident(snapshot.engine, name)).collect::<Vec<_>>().join(", ");
    let refs = foreign_key.ref_columns.iter().map(|name| quote_ident(snapshot.engine, name)).collect::<Vec<_>>().join(", ");
    let ref_table = dialect_of(snapshot.engine)?.fk_reference(snapshot, &foreign_key.ref_schema, &foreign_key.ref_table)?;
    Ok(format!(
        "ALTER TABLE {qualified} ADD CONSTRAINT {} FOREIGN KEY ({cols}) REFERENCES {ref_table} ({refs}) ON DELETE {} ON UPDATE {};",
        quote_ident(snapshot.engine, foreign_key.name.trim()),
        foreign_key.on_delete.trim(),
        foreign_key.on_update.trim()
    ))
}

pub(crate) fn recreate_index_sql(snapshot: &Snapshot, index: &Index, drafts: &[ColumnDraft]) -> Result<String, String> {
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

pub(crate) fn preservable_indexes(snapshot: &Snapshot, drafts: &[ColumnDraft]) -> Vec<Index> {
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

pub(crate) fn index_touches_change(index: &Index, snapshot: &Snapshot, drafts: &[ColumnDraft]) -> bool {
    index.columns.iter().any(|column| {
        drafts.iter().any(|draft| draft.original.as_deref() == Some(column.name.as_str()) && (draft.dropped || draft.name.trim() != column.name))
            || snapshot.columns.iter().any(|item| item.name == column.name)
                && drafts.iter().any(|draft| draft.dropped && draft.original.as_deref() == Some(column.name.as_str()))
    }) || ((index.partial || index.expression) && renamed_any(drafts))
}

pub(crate) fn column_referenced(snapshot: &Snapshot, name: &str) -> bool {
    snapshot.indexes.iter().any(|index| index.columns.iter().any(|column| column.name == name))
        || snapshot.foreign_keys.iter().any(|key| key.columns.iter().any(|column| column == name))
        || snapshot.incoming.iter().any(|key| key.ref_columns.iter().any(|column| column == name))
        || snapshot.triggers.iter().any(|trigger| mentions(&trigger.sql, name))
        || snapshot.views.iter().any(|view| mentions(&view.sql, name))
        || snapshot.columns.iter().any(|column| column.name == name && column.primary)
}

pub(crate) fn changed_names(snapshot: &Snapshot, drafts: &[ColumnDraft]) -> Vec<String> {
    drafts
        .iter()
        .filter_map(|draft| {
            let original = original_of(snapshot, draft)?;
            let changed = draft.dropped || draft.name.trim() != original.name || !same_type(&draft.type_name, &original.type_name);
            changed.then(|| original.name.clone())
        })
        .collect()
}

pub(crate) fn drafts_change_shape(snapshot: &Snapshot, drafts: &[ColumnDraft]) -> bool {
    drafts.iter().any(|draft| {
        original_of(snapshot, draft).is_none_or(|original| {
            draft.dropped || draft.name.trim() != original.name || !same_type(&draft.type_name, &original.type_name)
        })
    })
}

pub(crate) fn renamed_any(drafts: &[ColumnDraft]) -> bool {
    drafts.iter().any(|draft| draft.original.as_ref().is_some_and(|original| !draft.dropped && draft.name.trim() != original))
}

pub(crate) fn identity_drafts(snapshot: &Snapshot) -> Vec<ColumnDraft> {
    snapshot
        .columns
        .iter()
        .map(|column| ColumnDraft {
            original: Some(column.name.clone()),
            name: column.name.clone(),
            type_name: column.type_name.clone(),
            nullable: column.nullable,
            default: existing_default(snapshot.engine, column.default_sql.as_deref()),
            comment: String::new(),
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

pub(crate) fn original_of<'a>(snapshot: &'a Snapshot, draft: &ColumnDraft) -> Option<&'a Column> {
    let name = draft.original.as_deref()?;
    snapshot.columns.iter().find(|column| column.name == name)
}

pub(crate) fn same_type(left: &str, right: &str) -> bool {
    left.trim().eq_ignore_ascii_case(right.trim())
}

/// 草稿的默认值和读到的原列不同（按四态比较：数据库的写法差异不算变更）。
pub(crate) fn default_changed(engine: Engine, original: &Column, draft: &ColumnDraft) -> bool {
    draft.default != existing_default(engine, original.default_sql.as_deref())
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
        [statement] => classify_view_statement(snapshot.engine, statement),
        _ => ViewHead::Other,
    };
    if snapshot.materialized || matches!(&head, ViewHead::Materialized) {
        blocked.push("无法在不丢弃物化数据的情况下替换这个物化视图，因此不发送。".to_owned());
    }
    if ddl_of(snapshot.engine).is_some_and(|dialect| dialect.replace_view_blocked_by_dependents()) {
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
    let in_place = ddl_of(snapshot.engine).is_some_and(|dialect| dialect.replaces_view_in_place());
    let steps = if in_place {
        let sql = if let Some(sql) = bound {
            ensure_semicolon(&ensure_or_replace(&sql))
        } else {
            format!("CREATE OR REPLACE VIEW {qualified} AS\n{};", statement.trim().trim_end_matches(';').trim())
        };
        vec![Step { label: format!("替换视图 {table}"), sql }]
    } else {
        // 不能就地替换的方言先删后建。
        let create = if let Some(sql) = bound {
            ensure_semicolon(&sql)
        } else {
            format!("CREATE VIEW {qualified} AS\n{};", statement.trim().trim_end_matches(';').trim())
        };
        vec![
            Step { label: format!("删除视图 {table}"), sql: format!("DROP VIEW {qualified};") },
            Step { label: format!("创建视图 {table}"), sql: create },
        ]
    };
    finish(snapshot.engine, false, target, Vec::new(), risks, steps)
}

fn view_statements(input: &str) -> Vec<String> {
    // 字符串字面量与开闭相同的标识符引号里的分号不算语句结束。
    let symmetric_quotes: Vec<char> = crate::adapters::identifier_quotes().into_iter().filter(|(open, close)| open == close).map(|(open, _)| open).collect();
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
        if ch == '\'' || symmetric_quotes.contains(&ch) {
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

/// 视图语句头的记号（方言读自己的修饰时也用它）。
pub(crate) struct Tok {
    start: usize,
    end: usize,
    pub(crate) kind: TokKind,
}

pub(crate) enum TokKind {
    Word(String),
    Ident(String),
    Lit,
    Symbol(char),
}

fn classify_view_statement(engine: Engine, sql: &str) -> ViewHead {
    parse_view_head(engine, sql).unwrap_or(ViewHead::Other)
}

/// 通用的 CREATE [OR REPLACE] [TEMP] [RECURSIVE] VIEW 语句头；CREATE 与 VIEW 之间方言专属的修饰交给方言读。
fn parse_view_head(engine: Engine, sql: &str) -> Option<ViewHead> {
    let dialect = ddl_of(engine)?;
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
        // 方言不认识或写法不对的修饰：不当作可安全替换的视图语句。
        match dialect.view_modifier(&tokens, index)? {
            ViewModifier::Skip(next) => index = next,
            ViewModifier::Materialized(next) => {
                materialized = true;
                index = next;
            }
        }
        continue;
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
        Some(qualifier) => ddl_of(snapshot.engine)
            .is_some_and(|dialect| dialect.view_qualifiers(snapshot).into_iter().any(|catalog| ident_matches(snapshot.engine, qualifier, catalog))),
    }
}

/// 视图名与目标名比较：带引号的逐字相同；不带引号的按方言折叠（折成小写的方言要求目标全小写），否则不分大小写。
fn ident_matches(engine: Engine, ident: &SqlIdent, catalog: &str) -> bool {
    if catalog.is_empty() {
        return false;
    }
    if ident.quoted {
        return ident.text == catalog;
    }
    if adapter(engine).syntax().is_some_and(|syntax| syntax.folds_to_lowercase()) {
        return ident.text.to_ascii_lowercase() == catalog;
    }
    ident.text.eq_ignore_ascii_case(catalog)
}

pub(crate) fn word_is(token: &Tok, expected: &str) -> bool {
    matches!(&token.kind, TokKind::Word(text) if text.eq_ignore_ascii_case(expected))
}

pub(crate) fn symbol_is(token: &Tok, expected: char) -> bool {
    matches!(token.kind, TokKind::Symbol(ch) if ch == expected)
}

pub(crate) fn is_word(token: &Tok) -> bool {
    matches!(token.kind, TokKind::Word(_))
}

fn tokenize_sql(sql: &str) -> Vec<Tok> {
    let quotes = crate::adapters::identifier_quotes();
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
        let closer = if ch == '\'' { Some('\'') } else { quotes.iter().find(|(open, _)| *open == ch).map(|(_, close)| *close) };
        if let Some(closer) = closer {
            let start = index;
            let (kind, next) = read_quoted(sql, index, closer);
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

/// 读一个被引起来的记号：单引号是字符串字面量，其余是标识符；闭合符号连写两次是转义。
fn read_quoted(sql: &str, start: usize, closer: char) -> (TokKind, usize) {
    let opener = sql[start..].chars().next().unwrap();
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

/// 外键由连接上的开关控制的方言：说明当前开关，不静默改动它。
pub(crate) fn push_fk_status(snapshot: &Snapshot, risks: &mut Vec<Risk>) {
    if !ddl_of(snapshot.engine).is_some_and(|dialect| dialect.session_foreign_keys()) {
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

/// 计划目标说明：引擎 id 加上方言排列的库、模式与对象名。
pub(crate) fn target_label(engine: Engine, database: &str, schema: &str, table: &str) -> String {
    let id = adapter(engine).meta().id();
    match ddl_of(engine) {
        Some(dialect) => format!("{id} / {}", dialect.target_scope(database, schema, table)),
        None => format!("{id} / {table}"),
    }
}

pub(crate) fn finish(engine: Engine, rebuild: bool, target: String, blocked: Vec<String>, risks: Vec<Risk>, steps: Vec<Step>) -> Plan {
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
