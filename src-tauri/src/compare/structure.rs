//! 把对照改成基准的结构脚本。这里不连接数据库，也不执行语句。

use super::model::{
    Category, ColumnShape, DataKey, DiffItem, Engine, ForeignKeyShape, IndexShape, KeyChoice, Kind, ObjectDef, Side, StructureReport, TableChoice, TableShape,
};

pub fn diff_structure(baseline: &Side, target: &Side) -> StructureReport {
    if let Some(message) = refuse_pair(baseline, target) {
        return empty_report(Some(message));
    }
    let engine = match Engine::parse(&target.engine) {
        Ok(engine) => engine,
        Err(message) => return empty_report(Some(message)),
    };
    let mut notes = Vec::new();
    let mut incomplete = false;
    if baseline.cancelled || target.cancelled {
        incomplete = true;
        notes.push("对比已取消，未完成的结果不是完整脚本。".to_owned());
    }
    if !baseline.listing_complete || !target.listing_complete {
        incomplete = true;
        notes.push("对象名单没有读完，脚本不含创建或删除。".to_owned());
    }
    for (category, message) in baseline.category_errors.iter().chain(target.category_errors.iter()) {
        incomplete = true;
        notes.push(format!("{}读取失败：{message}", category.label()));
    }
    let allow_ddl = baseline.listing_complete && target.listing_complete;
    let mut only_baseline = Vec::new();
    let mut only_target = Vec::new();
    let mut changed = Vec::new();
    let mut drop_fk = Vec::new();
    let mut drop_index = Vec::new();
    let mut drop_object = Vec::new();
    let mut create_object = Vec::new();
    let mut create_index = Vec::new();
    let mut create_fk = Vec::new();
    let mut alters = Vec::new();

    let mut keys = std::collections::BTreeSet::new();
    for object in baseline.objects.iter().chain(target.objects.iter()) {
        keys.insert(identity(object));
    }
    for key in keys {
        let left = baseline.objects.iter().find(|object| identity(object) == key);
        let right = target.objects.iter().find(|object| identity(object) == key);
        match (left, right) {
            (Some(base), None) => {
                only_baseline.push(item(base, "仅基准拥有"));
                if allow_ddl && readable(base) && !blocked(baseline, target, Category::from_kind(base.kind)) {
                    push_create(engine, target, base, &mut create_object, &mut create_index, baseline, target);
                } else if base.read_error.is_some() {
                    incomplete = true;
                    notes.push(format!("{} {} 没有读全：{}", base.kind.label(), base.name, base.read_error.as_deref().unwrap_or("")));
                }
            }
            (None, Some(other)) => {
                only_target.push(item(other, "仅对照拥有"));
                if allow_ddl && readable(other) && !blocked(baseline, target, Category::from_kind(other.kind)) {
                    push_drop(engine, target, other, &mut drop_object, &mut drop_index, &mut drop_fk);
                } else if other.read_error.is_some() {
                    incomplete = true;
                    notes.push(format!("{} {} 没有读全：{}", other.kind.label(), other.name, other.read_error.as_deref().unwrap_or("")));
                }
            }
            (Some(base), Some(other)) => {
                if base.read_error.is_some() || other.read_error.is_some() {
                    incomplete = true;
                    let message = base.read_error.clone().or(other.read_error.clone()).unwrap_or_default();
                    notes.push(format!("{} {} 没有读全：{message}", base.kind.label(), base.name));
                    continue;
                }
                compare_pair(
                    engine,
                    target,
                    base,
                    other,
                    baseline,
                    target,
                    allow_ddl,
                    &mut changed,
                    &mut alters,
                    &mut drop_index,
                    &mut create_index,
                    &mut drop_fk,
                    &mut create_fk,
                    &mut drop_object,
                    &mut create_object,
                );
            }
            (None, None) => {}
        }
    }
    if blocked(baseline, target, Category::Index) {
        drop_index.clear();
        create_index.clear();
    }
    if blocked(baseline, target, Category::ForeignKey) {
        drop_fk.clear();
        create_fk.clear();
    }
    let mut statements = Vec::new();
    statements.extend(drop_fk);
    statements.extend(drop_index);
    statements.extend(drop_object);
    statements.extend(alters);
    statements.extend(create_object);
    statements.extend(create_index);
    statements.extend(create_fk);
    let tables = table_choices(&baseline.objects, &target.objects);
    notes.sort();
    notes.dedup();
    let unchanged = only_baseline.is_empty() && only_target.is_empty() && changed.is_empty();
    StructureReport {
        refused: None,
        script: render_script(&notes, &statements, incomplete, unchanged, "没有结构差异。"),
        incomplete,
        notes,
        only_baseline,
        only_target,
        changed,
        tables,
    }
}

fn empty_report(refused: Option<String>) -> StructureReport {
    StructureReport {
        refused,
        script: String::new(),
        incomplete: false,
        notes: Vec::new(),
        only_baseline: Vec::new(),
        only_target: Vec::new(),
        changed: Vec::new(),
        tables: Vec::new(),
    }
}

fn refuse_pair(baseline: &Side, target: &Side) -> Option<String> {
    let left = Engine::parse(&baseline.engine).ok();
    let right = Engine::parse(&target.engine).ok();
    if left.is_none() || right.is_none() || left != right {
        return Some("两侧引擎不同。MySQL 与 MariaDB 是不同引擎，不生成脚本。".to_owned());
    }
    if !baseline.connected || !target.connected {
        return Some("有一侧还没有打开，不生成脚本。".to_owned());
    }
    if let Some(message) = baseline.fatal.clone().or(target.fatal.clone()) {
        return Some(format!("{message} 不生成脚本。"));
    }
    if left == Some(Engine::Postgres) && (baseline.schema.trim().is_empty() || target.schema.trim().is_empty()) {
        return Some("PostgreSQL 需要为基准和对照各选一个 schema，不生成脚本。".to_owned());
    }
    if left == Some(Engine::Sqlite) && ((!baseline.database.is_empty() && baseline.database != "main") || (!target.database.is_empty() && target.database != "main")) {
        return Some("SQLite 只对比该连接的 main，不附加其他文件，不生成脚本。".to_owned());
    }
    None
}

fn identity(object: &ObjectDef) -> String {
    match object.kind {
        Kind::Function | Kind::Procedure => format!("{:?}\u{1}{}\u{1}{}", object.kind, object.name, object.signature),
        Kind::Trigger => format!("trigger\u{1}{}\u{1}{}", object.name, object.relation),
        _ => format!("{:?}\u{1}{}", object.kind, object.name),
    }
}

fn item(object: &ObjectDef, detail: &str) -> DiffItem {
    DiffItem { kind: object.kind.label().to_owned(), name: display_name(object), detail: detail.to_owned() }
}

fn display_name(object: &ObjectDef) -> String {
    match object.kind {
        Kind::Function | Kind::Procedure if !object.signature.is_empty() => format!("{}({})", object.name, object.signature),
        Kind::Trigger if !object.relation.is_empty() => format!("{} on {}", object.name, object.relation),
        _ => object.name.clone(),
    }
}

fn readable(object: &ObjectDef) -> bool {
    object.read_error.is_none()
}

fn blocked(baseline: &Side, target: &Side, category: Category) -> bool {
    let table_failed = baseline.category_errors.iter().chain(target.category_errors.iter()).any(|(item, _)| *item == Category::Table);
    if table_failed && matches!(category, Category::Table | Category::Index | Category::ForeignKey) {
        return true;
    }
    baseline.category_errors.iter().chain(target.category_errors.iter()).any(|(item, _)| *item == category)
}

fn push_create(engine: Engine, target: &Side, object: &ObjectDef, objects: &mut Vec<String>, indexes: &mut Vec<String>, baseline: &Side, other: &Side) {
    match object.kind {
        Kind::Table => {
            let Some(shape) = object.table.as_ref() else {
                return;
            };
            objects.push(create_table(engine, target, &object.name, shape, !blocked(baseline, other, Category::ForeignKey)));
            if !blocked(baseline, other, Category::Index) {
                indexes.extend(shape.indexes.iter().filter(|index| !skip_index(index, shape)).map(|index| create_index(engine, target, &object.name, index)));
            }
        }
        _ => {
            if let Some(sql) = create_definition(engine, target, object) {
                objects.push(sql);
            }
        }
    }
}

fn push_drop(engine: Engine, target: &Side, object: &ObjectDef, objects: &mut Vec<String>, indexes: &mut Vec<String>, foreign_keys: &mut Vec<String>) {
    let _ = (indexes, foreign_keys);
    objects.push(drop_object(engine, target, object));
}

fn compare_pair(
    engine: Engine,
    target: &Side,
    base: &ObjectDef,
    other: &ObjectDef,
    baseline: &Side,
    target_side: &Side,
    allow_ddl: bool,
    changed: &mut Vec<DiffItem>,
    alters: &mut Vec<String>,
    index_drops: &mut Vec<String>,
    index_creates: &mut Vec<String>,
    drop_fk: &mut Vec<String>,
    create_fk: &mut Vec<String>,
    object_drops: &mut Vec<String>,
    create_object: &mut Vec<String>,
) {
    if base.kind != Kind::Table {
        if base.definition != other.definition || base.materialized != other.materialized {
            changed.push(item(base, "定义文本不同"));
            if allow_ddl && !blocked(baseline, target_side, Category::from_kind(base.kind)) {
                object_drops.push(drop_object(engine, target, other));
                if let Some(sql) = create_definition(engine, target, base) {
                    create_object.push(sql);
                }
            }
        }
        return;
    }
    let (Some(left), Some(right)) = (base.table.as_ref(), other.table.as_ref()) else {
        changed.push(item(base, "没有读到表结构"));
        return;
    };
    let column_diff = columns_differ(left, right);
    // SQLite 的主键来自列信息；其他引擎的主键来自索引查询。
    let pk_trusted = engine == Engine::Sqlite || (left.indexes_read && right.indexes_read);
    let pk_diff = pk_trusted && primary_columns(left) != primary_columns(right);
    let fk_known = left.foreign_keys_read && right.foreign_keys_read;
    let fk_diff = fk_known && foreign_keys_differ(left, right);
    let with_fk = fk_known && !blocked(baseline, target_side, Category::ForeignKey);
    let index_blocked = blocked(baseline, target_side, Category::Index) || !left.indexes_read || !right.indexes_read;
    let sqlite_fk_only = engine == Engine::Sqlite && fk_diff && !column_diff && !pk_diff;
    let structural = engine == Engine::Sqlite && sqlite_needs_rebuild(left, right, with_fk);
    // 仅外键不同时 SQLite 只能重建。索引没读全时重建会连带丢掉索引，因此不生成这类语句。
    let emit_alter = allow_ddl && !blocked(baseline, target_side, Category::Table) && !(structural && index_blocked) && (column_diff || pk_diff || (sqlite_fk_only && with_fk && !index_blocked));
    if column_diff || pk_diff {
        changed.push(item(base, if column_diff { "列的名称、类型、可否为空或默认值不同" } else { "主键不同" }));
    }
    if emit_alter {
        alters.extend(alter_table(engine, target, &base.name, left, right, with_fk, pk_trusted));
    }
    let rebuilt = emit_alter && engine == Engine::Sqlite && sqlite_needs_rebuild(left, right, with_fk);
    if left.indexes_read && right.indexes_read {
        compare_indexes(
            engine,
            target,
            &base.name,
            left,
            right,
            changed,
            index_drops,
            index_creates,
            allow_ddl && !index_blocked && !rebuilt,
        );
    }
    if rebuilt && !index_blocked {
        index_creates.extend(left.indexes.iter().filter(|index| !skip_index(index, left)).map(|index| create_index(engine, target, &base.name, index)));
    }
    if fk_diff {
        changed.push(DiffItem { kind: "外键".to_owned(), name: base.name.clone(), detail: "外键不同".to_owned() });
        if allow_ddl && with_fk && engine != Engine::Sqlite {
            for key in &right.foreign_keys {
                if !left.foreign_keys.iter().any(|item| item.name == key.name && item == key) {
                    drop_fk.push(drop_foreign_key(engine, target, &base.name, key));
                }
            }
            for key in &left.foreign_keys {
                if !right.foreign_keys.iter().any(|item| item == key) {
                    create_fk.push(add_foreign_key(engine, target, &base.name, key));
                }
            }
        }
    }
}

fn columns_differ(left: &TableShape, right: &TableShape) -> bool {
    let mut names = std::collections::BTreeSet::new();
    names.extend(left.columns.iter().map(|column| column.name.clone()));
    names.extend(right.columns.iter().map(|column| column.name.clone()));
    for name in names {
        match (left.columns.iter().find(|column| column.name == name), right.columns.iter().find(|column| column.name == name)) {
            (Some(base), Some(other)) => {
                if !base.type_name.eq_ignore_ascii_case(&other.type_name) || base.nullable != other.nullable || base.default_sql.trim() != other.default_sql.trim() {
                    return true;
                }
            }
            _ => return true,
        }
    }
    false
}

fn primary_columns(shape: &TableShape) -> Vec<String> {
    if let Some(index) = shape.indexes.iter().find(|index| index.reason.contains("主键")) {
        return index.columns.iter().map(|(name, _)| name.clone()).collect();
    }
    shape.columns.iter().filter(|column| column.primary).map(|column| column.name.clone()).collect()
}

fn foreign_keys_differ(left: &TableShape, right: &TableShape) -> bool {
    let normalize = |keys: &[ForeignKeyShape]| {
        let mut items = keys.to_vec();
        items.sort_by(|left, right| left.name.cmp(&right.name));
        items
    };
    normalize(&left.foreign_keys) != normalize(&right.foreign_keys)
}

fn compare_indexes(
    engine: Engine,
    target: &Side,
    table: &str,
    left: &TableShape,
    right: &TableShape,
    changed: &mut Vec<DiffItem>,
    index_drops: &mut Vec<String>,
    index_creates: &mut Vec<String>,
    allow_ddl: bool,
) {
    let usable = |shape: &TableShape, index: &IndexShape| !skip_index(index, shape);
    let mut names = std::collections::BTreeSet::new();
    names.extend(left.indexes.iter().filter(|index| usable(left, index)).map(|index| index.name.clone()));
    names.extend(right.indexes.iter().filter(|index| usable(right, index)).map(|index| index.name.clone()));
    for name in names {
        let base = left.indexes.iter().find(|index| index.name == name);
        let other = right.indexes.iter().find(|index| index.name == name);
        match (base, other) {
            (Some(base), None) => {
                changed.push(DiffItem { kind: "索引".to_owned(), name: name.clone(), detail: format!("仅基准拥有，属于表 {table}") });
                if allow_ddl {
                    index_creates.push(create_index(engine, target, table, base));
                }
            }
            (None, Some(other)) => {
                changed.push(DiffItem { kind: "索引".to_owned(), name: name.clone(), detail: format!("仅对照拥有，属于表 {table}") });
                if allow_ddl {
                    index_drops.push(drop_index(engine, target, table, other));
                }
            }
            (Some(base), Some(other)) if !same_index(base, other) => {
                changed.push(DiffItem { kind: "索引".to_owned(), name: name.clone(), detail: format!("表 {table} 的唯一性、列顺序或定义文本不同") });
                if allow_ddl {
                    index_drops.push(drop_index(engine, target, table, other));
                    index_creates.push(create_index(engine, target, table, base));
                }
            }
            _ => {}
        }
    }
}

fn same_index(left: &IndexShape, right: &IndexShape) -> bool {
    left.unique == right.unique && left.columns == right.columns && left.partial == right.partial && left.expression == right.expression && left.sql == right.sql
}

fn skip_index(index: &IndexShape, shape: &TableShape) -> bool {
    if index.name.starts_with("sqlite_autoindex") || index.reason.contains("主键") {
        return true;
    }
    let pk = primary_columns(shape);
    !pk.is_empty() && index.columns.iter().map(|(name, _)| name.as_str()).eq(pk.iter().map(String::as_str))
}

fn quote(engine: Engine, name: &str) -> String {
    let mark = match engine {
        Engine::Mysql | Engine::Mariadb => '`',
        Engine::Postgres | Engine::Sqlite => '"',
    };
    format!("{mark}{}{mark}", name.replace(mark, &format!("{mark}{mark}")))
}

fn qualify(engine: Engine, side: &Side, name: &str) -> String {
    match engine {
        Engine::Sqlite => quote(engine, name),
        Engine::Mysql | Engine::Mariadb => format!("{}.{}", quote(engine, &side.database), quote(engine, name)),
        Engine::Postgres => format!("{}.{}", quote(engine, &side.schema), quote(engine, name)),
    }
}

fn column_clause(engine: Engine, column: &ColumnShape) -> String {
    let mut line = format!("{} {}", quote(engine, &column.name), column.type_name.trim());
    if !column.nullable {
        line.push_str(" NOT NULL");
    }
    if !column.default_sql.trim().is_empty() {
        line.push_str(" DEFAULT ");
        line.push_str(column.default_sql.trim());
    }
    line
}

fn create_table(engine: Engine, side: &Side, name: &str, shape: &TableShape, with_fk: bool) -> String {
    let mut lines = Vec::new();
    for column in &shape.columns {
        lines.push(column_clause(engine, column));
    }
    let pk = primary_columns(shape);
    if !pk.is_empty() {
        let names = pk.iter().map(|column| quote(engine, column)).collect::<Vec<_>>().join(", ");
        lines.push(format!("PRIMARY KEY ({names})"));
    }
    if with_fk {
        for key in &shape.foreign_keys {
            lines.push(foreign_key_clause(engine, key));
        }
    }
    format!("CREATE TABLE {} (\n  {}\n)", qualify(engine, side, name), lines.join(",\n  "))
}

fn foreign_key_clause(engine: Engine, key: &ForeignKeyShape) -> String {
    let columns = key.columns.iter().map(|column| quote(engine, column)).collect::<Vec<_>>().join(", ");
    let refs = key.ref_columns.iter().map(|column| quote(engine, column)).collect::<Vec<_>>().join(", ");
    let mut clause = format!("CONSTRAINT {} FOREIGN KEY ({columns}) REFERENCES {} ({refs})", quote(engine, &key.name), reference_name(engine, key));
    if !key.on_delete.trim().is_empty() {
        clause.push_str(" ON DELETE ");
        clause.push_str(key.on_delete.trim());
    }
    if !key.on_update.trim().is_empty() {
        clause.push_str(" ON UPDATE ");
        clause.push_str(key.on_update.trim());
    }
    clause
}

fn reference_name(engine: Engine, key: &ForeignKeyShape) -> String {
    if key.ref_schema.is_empty() {
        quote(engine, &key.ref_table)
    } else {
        format!("{}.{}", quote(engine, &key.ref_schema), quote(engine, &key.ref_table))
    }
}

fn create_index(engine: Engine, side: &Side, table: &str, index: &IndexShape) -> String {
    if (index.partial || index.expression) && !index.sql.trim().is_empty() {
        return retarget_index_sql(engine, side, table, index.sql.trim().trim_end_matches(';'));
    }
    let unique = if index.unique { "UNIQUE " } else { "" };
    let columns = if index.columns.is_empty() {
        index.sql.trim().trim_end_matches(';').to_owned()
    } else {
        let list = index
            .columns
            .iter()
            .map(|(name, descending)| format!("{} {}", quote(engine, name), if *descending { "DESC" } else { "ASC" }))
            .collect::<Vec<_>>()
            .join(", ");
        format!("CREATE {unique}INDEX {} ON {} ({list})", quote(engine, &index.name), qualify(engine, side, table))
    };
    columns
}

fn drop_index(engine: Engine, side: &Side, table: &str, index: &IndexShape) -> String {
    match engine {
        Engine::Mysql | Engine::Mariadb => format!("DROP INDEX {} ON {}", quote(engine, &index.name), qualify(engine, side, table)),
        Engine::Postgres => format!("DROP INDEX {}.{}", quote(engine, &side.schema), quote(engine, &index.name)),
        Engine::Sqlite => format!("DROP INDEX {}", quote(engine, &index.name)),
    }
}

fn add_foreign_key(engine: Engine, side: &Side, table: &str, key: &ForeignKeyShape) -> String {
    format!("ALTER TABLE {} ADD {}", qualify(engine, side, table), foreign_key_clause(engine, key))
}

fn drop_foreign_key(engine: Engine, side: &Side, table: &str, key: &ForeignKeyShape) -> String {
    match engine {
        Engine::Mysql | Engine::Mariadb => format!("ALTER TABLE {} DROP FOREIGN KEY {}", qualify(engine, side, table), quote(engine, &key.name)),
        _ => format!("ALTER TABLE {} DROP CONSTRAINT {}", qualify(engine, side, table), quote(engine, &key.name)),
    }
}

fn drop_object(engine: Engine, side: &Side, object: &ObjectDef) -> String {
    let name = qualify(engine, side, &object.name);
    match object.kind {
        Kind::Table => format!("DROP TABLE {name}"),
        Kind::View if object.materialized => format!("DROP MATERIALIZED VIEW {name}"),
        Kind::View => format!("DROP VIEW {name}"),
        Kind::Sequence => format!("DROP SEQUENCE {name}"),
        Kind::Enum => format!("DROP TYPE {name}"),
        Kind::Domain => format!("DROP DOMAIN {name}"),
        Kind::Function => drop_routine(engine, side, "FUNCTION", object),
        Kind::Procedure => drop_routine(engine, side, "PROCEDURE", object),
        Kind::Trigger => match engine {
            Engine::Postgres => format!("DROP TRIGGER {} ON {}", quote(engine, &object.name), qualify(engine, side, &object.relation)),
            Engine::Mysql | Engine::Mariadb => format!("DROP TRIGGER {}", qualify(engine, side, &object.name)),
            Engine::Sqlite => format!("DROP TRIGGER {}", quote(engine, &object.name)),
        },
    }
}

fn drop_routine(engine: Engine, side: &Side, word: &str, object: &ObjectDef) -> String {
    match engine {
        Engine::Postgres if !object.signature.is_empty() => format!("DROP {word} {}.{}({})", quote(engine, &side.schema), quote(engine, &object.name), object.signature),
        Engine::Postgres => format!("DROP {word} {}.{}", quote(engine, &side.schema), quote(engine, &object.name)),
        Engine::Sqlite => format!("DROP {word} {}", quote(engine, &object.name)),
        Engine::Mysql | Engine::Mariadb => format!("DROP {word} {}", qualify(engine, side, &object.name)),
    }
}

fn retarget_create(engine: Engine, side: &Side, object: &ObjectDef, definition: &str) -> String {
    let keyword = match object.kind {
        Kind::View => "VIEW",
        Kind::Sequence => "SEQUENCE",
        Kind::Enum => "TYPE",
        Kind::Domain => "DOMAIN",
        Kind::Function => "FUNCTION",
        Kind::Procedure => "PROCEDURE",
        Kind::Trigger => "TRIGGER",
        Kind::Table => return definition.to_owned(),
    };
    let mut sql = replace_name_after(definition, keyword, &qualify(engine, side, &object.name)).unwrap_or_else(|| definition.to_owned());
    if object.kind == Kind::Trigger && !object.relation.is_empty() {
        if let Some(rewritten) = replace_name_after(&sql, "ON", &qualify(engine, side, &object.relation)) {
            sql = rewritten;
        }
    }
    sql
}

fn retarget_index_sql(engine: Engine, side: &Side, table: &str, sql: &str) -> String {
    replace_name_after(sql, "ON", &qualify(engine, side, table)).unwrap_or_else(|| sql.to_owned())
}

fn replace_name_after(sql: &str, keyword: &str, replacement: &str) -> Option<String> {
    let mut index = find_keyword_end(sql, keyword)?;
    index = skip_gap(sql, index);
    if starts_with_word(sql, index, "IF") {
        let after_if = skip_word(sql, index);
        let not_at = skip_gap(sql, after_if);
        if starts_with_word(sql, not_at, "NOT") {
            let exists_at = skip_gap(sql, skip_word(sql, not_at));
            if starts_with_word(sql, exists_at, "EXISTS") {
                index = skip_gap(sql, skip_word(sql, exists_at));
            }
        }
    }
    if starts_with_word(sql, index, "ONLY") {
        index = skip_gap(sql, skip_word(sql, index));
    }
    let start = skip_gap(sql, index);
    let end = qualified_name_end(sql, start)?;
    Some(format!("{}{}{}", &sql[..start], replacement, &sql[end..]))
}

fn find_keyword_end(sql: &str, keyword: &str) -> Option<usize> {
    let mut index = 0;
    while index < sql.len() {
        if let Some(next) = skip_delimited(sql, index) {
            index = next;
            continue;
        }
        let ch = sql[index..].chars().next()?;
        if ch.is_whitespace() {
            index += ch.len_utf8();
            continue;
        }
        if is_ident_start(ch) {
            let start = index;
            index = ident_continue(sql, index);
            if sql[start..index].eq_ignore_ascii_case(keyword) {
                return Some(index);
            }
            continue;
        }
        index += ch.len_utf8();
    }
    None
}

fn qualified_name_end(sql: &str, start: usize) -> Option<usize> {
    let end = ident_end(sql, start)?;
    let mut index = skip_gap(sql, end);
    if sql[index..].starts_with('.') {
        index = skip_gap(sql, index + 1);
        return ident_end(sql, index);
    }
    Some(end)
}

fn ident_end(sql: &str, start: usize) -> Option<usize> {
    if start >= sql.len() {
        return None;
    }
    let ch = sql[start..].chars().next()?;
    if ch == '"' || ch == '`' || ch == '[' {
        let closer = if ch == '[' { ']' } else { ch };
        return skip_quoted(sql, start, closer);
    }
    if !is_ident_start(ch) {
        return None;
    }
    Some(ident_continue(sql, start))
}

fn ident_continue(sql: &str, start: usize) -> usize {
    let mut index = start;
    let mut first = true;
    while index < sql.len() {
        let ch = sql[index..].chars().next().unwrap();
        let ok = if first { is_ident_start(ch) } else { is_ident_continue(ch) };
        if !ok {
            break;
        }
        first = false;
        index += ch.len_utf8();
    }
    index
}

fn starts_with_word(sql: &str, index: usize, word: &str) -> bool {
    let end = ident_continue(sql, index);
    end > index && sql[index..end].eq_ignore_ascii_case(word)
}

fn skip_word(sql: &str, index: usize) -> usize {
    ident_continue(sql, index)
}

fn skip_gap(sql: &str, mut index: usize) -> usize {
    while index < sql.len() {
        let rest = &sql[index..];
        if rest.starts_with("--") {
            index += rest.find('\n').map(|at| at + 1).unwrap_or(rest.len());
            continue;
        }
        if rest.starts_with("/*") {
            index = skip_block_comment(sql, index);
            continue;
        }
        let ch = rest.chars().next().unwrap();
        if ch.is_whitespace() {
            index += ch.len_utf8();
            continue;
        }
        break;
    }
    index
}

fn skip_delimited(sql: &str, index: usize) -> Option<usize> {
    let rest = sql.get(index..)?;
    if rest.starts_with("--") {
        return Some(index + rest.find('\n').map(|at| at + 1).unwrap_or(rest.len()));
    }
    if rest.starts_with("/*") {
        return Some(skip_block_comment(sql, index));
    }
    let ch = rest.chars().next()?;
    if ch == '\'' || ch == '"' || ch == '`' {
        return skip_quoted(sql, index, ch);
    }
    if ch == '$' {
        return skip_dollar(sql, index);
    }
    None
}

fn skip_block_comment(sql: &str, start: usize) -> usize {
    let mut index = start + 2;
    let mut depth = 1;
    while index < sql.len() {
        if sql[index..].starts_with("/*") {
            depth += 1;
            index += 2;
            continue;
        }
        if sql[index..].starts_with("*/") {
            depth -= 1;
            index += 2;
            if depth == 0 {
                return index;
            }
            continue;
        }
        index += sql[index..].chars().next().unwrap().len_utf8();
    }
    sql.len()
}

fn skip_quoted(sql: &str, start: usize, closer: char) -> Option<usize> {
    let mut index = start + closer.len_utf8();
    if closer == ']' {
        index = start + 1;
    }
    while index < sql.len() {
        let ch = sql[index..].chars().next()?;
        if ch == closer {
            let next = index + ch.len_utf8();
            if sql[next..].starts_with(closer) {
                index = next + ch.len_utf8();
                continue;
            }
            return Some(next);
        }
        index += ch.len_utf8();
    }
    Some(sql.len())
}

fn skip_dollar(sql: &str, start: usize) -> Option<usize> {
    let rest = sql.get(start..)?;
    let tag_end = rest[1..].find('$')?;
    let delimiter = &rest[..=tag_end + 1];
    if !delimiter[1..delimiter.len() - 1].chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return None;
    }
    let body = start + delimiter.len();
    sql[body..].find(delimiter).map(|at| body + at + delimiter.len())
}

fn is_ident_start(ch: char) -> bool {
    ch.is_alphabetic() || ch == '_'
}

fn is_ident_continue(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$'
}

fn create_definition(engine: Engine, side: &Side, object: &ObjectDef) -> Option<String> {
    let definition = object.definition.trim();
    if definition.is_empty() || definition.starts_with("--") {
        return None;
    }
    if definition.len() >= 6 && definition[..6].eq_ignore_ascii_case("CREATE") {
        return Some(retarget_create(engine, side, object, definition.trim_end_matches(';').trim()));
    }
    let name = qualify(engine, side, &object.name);
    let sql = match object.kind {
        Kind::View if object.materialized => format!("CREATE MATERIALIZED VIEW {name} AS {definition}"),
        Kind::View => format!("CREATE VIEW {name} AS {definition}"),
        _ => definition.to_owned(),
    };
    Some(sql)
}

fn alter_table(engine: Engine, side: &Side, name: &str, baseline: &TableShape, target: &TableShape, with_fk: bool, with_pk: bool) -> Vec<String> {
    match engine {
        Engine::Sqlite => sqlite_alter(side, name, baseline, target, with_fk),
        Engine::Postgres => postgres_alter(side, name, baseline, target, with_pk),
        Engine::Mysql | Engine::Mariadb => mysql_alter(engine, side, name, baseline, target, with_pk),
    }
}

fn postgres_alter(side: &Side, name: &str, baseline: &TableShape, target: &TableShape, with_pk: bool) -> Vec<String> {
    let engine = Engine::Postgres;
    let table = qualify(engine, side, name);
    let mut statements = Vec::new();
    for column in &target.columns {
        if !baseline.columns.iter().any(|item| item.name == column.name) {
            statements.push(format!("ALTER TABLE {table} DROP COLUMN {}", quote(engine, &column.name)));
        }
    }
    for column in &baseline.columns {
        match target.columns.iter().find(|item| item.name == column.name) {
            None => statements.push(format!("ALTER TABLE {table} ADD COLUMN {}", column_clause(engine, column))),
            Some(other) => {
                if !column.type_name.eq_ignore_ascii_case(&other.type_name) {
                    statements.push(format!("ALTER TABLE {table} ALTER COLUMN {} TYPE {}", quote(engine, &column.name), column.type_name.trim()));
                }
                if column.nullable != other.nullable {
                    let verb = if column.nullable { "DROP NOT NULL" } else { "SET NOT NULL" };
                    statements.push(format!("ALTER TABLE {table} ALTER COLUMN {} {verb}", quote(engine, &column.name)));
                }
                if column.default_sql.trim() != other.default_sql.trim() {
                    if column.default_sql.trim().is_empty() {
                        statements.push(format!("ALTER TABLE {table} ALTER COLUMN {} DROP DEFAULT", quote(engine, &column.name)));
                    } else {
                        statements.push(format!("ALTER TABLE {table} ALTER COLUMN {} SET DEFAULT {}", quote(engine, &column.name), column.default_sql.trim()));
                    }
                }
            }
        }
    }
    if with_pk {
        push_primary_key(engine, &table, name, baseline, target, &mut statements);
    }
    statements
}

fn push_primary_key(engine: Engine, table: &str, name: &str, baseline: &TableShape, target: &TableShape, statements: &mut Vec<String>) {
    let wanted = primary_columns(baseline);
    let current = primary_columns(target);
    if wanted == current {
        return;
    }
    match engine {
        Engine::Postgres => {
            if !current.is_empty() {
                let constraint = primary_key_name(target).unwrap_or_else(|| format!("{name}_pkey"));
                statements.push(format!("ALTER TABLE {table} DROP CONSTRAINT {}", quote(engine, &constraint)));
            }
            if !wanted.is_empty() {
                let names = wanted.iter().map(|column| quote(engine, column)).collect::<Vec<_>>().join(", ");
                statements.push(format!("ALTER TABLE {table} ADD PRIMARY KEY ({names})"));
            }
        }
        Engine::Mysql | Engine::Mariadb => {
            let names = wanted.iter().map(|column| quote(engine, column)).collect::<Vec<_>>().join(", ");
            match (!current.is_empty(), !wanted.is_empty()) {
                (true, true) => statements.push(format!("ALTER TABLE {table} DROP PRIMARY KEY, ADD PRIMARY KEY ({names})")),
                (true, false) => statements.push(format!("ALTER TABLE {table} DROP PRIMARY KEY")),
                (false, true) => statements.push(format!("ALTER TABLE {table} ADD PRIMARY KEY ({names})")),
                (false, false) => {}
            }
        }
        Engine::Sqlite => {}
    }
}

fn primary_key_name(shape: &TableShape) -> Option<String> {
    shape.indexes.iter().find(|index| index.reason.contains("主键")).map(|index| index.name.clone())
}

fn mysql_alter(engine: Engine, side: &Side, name: &str, baseline: &TableShape, target: &TableShape, with_pk: bool) -> Vec<String> {
    let table = qualify(engine, side, name);
    let mut statements = Vec::new();
    for column in &target.columns {
        if !baseline.columns.iter().any(|item| item.name == column.name) {
            statements.push(format!("ALTER TABLE {table} DROP COLUMN {}", quote(engine, &column.name)));
        }
    }
    for column in &baseline.columns {
        match target.columns.iter().find(|item| item.name == column.name) {
            None => statements.push(format!("ALTER TABLE {table} ADD COLUMN {}", column_clause(engine, column))),
            Some(other) if !column.type_name.eq_ignore_ascii_case(&other.type_name) || column.nullable != other.nullable || column.default_sql.trim() != other.default_sql.trim() => {
                statements.push(format!("ALTER TABLE {table} MODIFY COLUMN {}", column_clause(engine, column)));
            }
            _ => {}
        }
    }
    if with_pk {
        push_primary_key(engine, &table, name, baseline, target, &mut statements);
    }
    statements
}

fn sqlite_needs_rebuild(baseline: &TableShape, target: &TableShape, with_fk: bool) -> bool {
    primary_columns(baseline) != primary_columns(target)
        || baseline.columns.iter().any(|column| {
            target.columns.iter().any(|other| other.name == column.name && (!column.type_name.eq_ignore_ascii_case(&other.type_name) || column.nullable != other.nullable || column.default_sql.trim() != other.default_sql.trim()))
        })
        || (with_fk && foreign_keys_differ(baseline, target))
}

fn sqlite_alter(side: &Side, name: &str, baseline: &TableShape, target: &TableShape, with_fk: bool) -> Vec<String> {
    let engine = Engine::Sqlite;
    let structural = sqlite_needs_rebuild(baseline, target, with_fk);
    if !structural {
        let table = qualify(engine, side, name);
        let mut statements = Vec::new();
        for column in &target.columns {
            if !baseline.columns.iter().any(|item| item.name == column.name) {
                statements.push(format!("ALTER TABLE {table} DROP COLUMN {}", quote(engine, &column.name)));
            }
        }
        for column in &baseline.columns {
            if !target.columns.iter().any(|item| item.name == column.name) {
                statements.push(format!("ALTER TABLE {table} ADD COLUMN {}", column_clause(engine, column)));
            }
        }
        return statements;
    }
    let fresh = format!("{name}__rillsql_new");
    let shared = baseline
        .columns
        .iter()
        .filter(|column| target.columns.iter().any(|other| other.name == column.name))
        .map(|column| quote(engine, &column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let mut statements = vec![create_table(engine, side, &fresh, baseline, with_fk)];
    if !shared.is_empty() {
        statements.push(format!("INSERT INTO {} ({shared}) SELECT {shared} FROM {}", quote(engine, &fresh), quote(engine, name)));
    }
    statements.push(format!("DROP TABLE {}", quote(engine, name)));
    statements.push(format!("ALTER TABLE {} RENAME TO {}", quote(engine, &fresh), quote(engine, name)));
    statements
}

fn table_choices(baseline: &[ObjectDef], target: &[ObjectDef]) -> Vec<TableChoice> {
    let mut names = std::collections::BTreeSet::new();
    for object in baseline.iter().chain(target.iter()).filter(|object| object.kind == Kind::Table) {
        names.insert(object.name.clone());
    }
    names
        .into_iter()
        .map(|name| {
            let left = baseline.iter().find(|object| object.kind == Kind::Table && object.name == name);
            let right = target.iter().find(|object| object.kind == Kind::Table && object.name == name);
            let shape = right.and_then(|object| object.table.as_ref()).or(left.and_then(|object| object.table.as_ref()));
            let keys = shape.map(key_choices).unwrap_or_default();
            let unique = keys.iter().filter(|key| !key.primary).count();
            TableChoice {
                name,
                on_both: left.is_some() && right.is_some(),
                needs_key_choice: keys.iter().all(|key| !key.primary) && unique > 1,
                keys,
            }
        })
        .collect()
}

pub fn keys_from_shape(shape: &TableShape) -> Vec<DataKey> {
    key_choices(shape)
        .into_iter()
        .map(|key| DataKey { name: key.name, columns: key.columns, primary: key.primary })
        .collect()
}

fn key_choices(shape: &TableShape) -> Vec<KeyChoice> {
    let mut keys = Vec::new();
    let pk = primary_columns(shape);
    if !pk.is_empty() {
        let name = shape.indexes.iter().find(|index| index.reason.contains("主键")).map(|index| index.name.clone()).unwrap_or_else(|| "PRIMARY".to_owned());
        keys.push(KeyChoice { name, columns: pk.clone(), primary: true });
    }
    for index in &shape.indexes {
        if skip_index(index, shape) || !index.unique || index.partial || index.expression || index.columns.is_empty() {
            continue;
        }
        let columns = index.columns.iter().map(|(name, _)| name.clone()).collect::<Vec<_>>();
        if columns == pk {
            continue;
        }
        keys.push(KeyChoice { name: index.name.clone(), columns, primary: false });
    }
    keys
}

pub(super) fn render_script(notes: &[String], statements: &[String], incomplete: bool, unchanged: bool, empty_message: &str) -> String {
    let mut lines = vec!["-- 结构差异脚本。生成时没有执行。".to_owned()];
    if incomplete {
        lines.push("-- 脚本不完整。".to_owned());
    }
    for note in notes {
        lines.push(format!("-- {note}"));
    }
    if statements.is_empty() {
        if incomplete {
            lines.push("-- 没有可放入完整脚本的变更语句。".to_owned());
        } else if unchanged {
            lines.push(format!("-- {empty_message}"));
        }
    }
    for statement in statements {
        let text = statement.trim().trim_end_matches(';').trim();
        if text.is_empty() {
            continue;
        }
        lines.push(format!("{text};"));
    }
    lines.join("\n")
}
