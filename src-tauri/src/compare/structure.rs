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
    let mut drop_object: Vec<PlannedDrop> = Vec::new();
    let mut create_object: Vec<PlannedCreate> = Vec::new();
    let mut create_index = Vec::new();
    let mut create_fk = Vec::new();
    let mut alters = Vec::new();
    let mut in_place = Vec::new();
    let mut detach = Vec::new();
    let mut restore = Vec::new();

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
                    push_create(engine, target, base, &mut create_object, &mut create_index, &mut create_fk, baseline, target);
                } else if base.read_error.is_some() {
                    incomplete = true;
                    notes.push(format!("{} {} 没有读全：{}", base.kind.label(), base.name, base.read_error.as_deref().unwrap_or("")));
                }
            }
            (None, Some(other)) => {
                only_target.push(item(other, "仅对照拥有"));
                if allow_ddl && readable(other) && !blocked(baseline, target, Category::from_kind(other.kind)) {
                    let drop_owned_fk = engine != Engine::Sqlite && !blocked(baseline, target, Category::ForeignKey);
                    push_drop(engine, target, other, &mut drop_object, &mut drop_fk, drop_owned_fk);
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
                    &mut notes,
                    &mut incomplete,
                );
            }
            (None, None) => {}
        }
    }
    preserve_dependents(engine, baseline, target, &mut drop_object, &mut create_object, &alters, &mut in_place, &mut detach, &mut restore);
    if blocked(baseline, target, Category::Index) {
        drop_index.clear();
        create_index.clear();
    }
    if blocked(baseline, target, Category::ForeignKey) {
        drop_fk.clear();
        create_fk.clear();
    }
    let statements = assemble_statements(
        engine,
        target,
        drop_fk,
        drop_index,
        drop_object,
        alters,
        create_object,
        create_index,
        create_fk,
        in_place,
        detach,
        restore,
    );
    let tables = table_choices(engine, &baseline.objects, &target.objects);
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

fn push_create(
    engine: Engine,
    target: &Side,
    object: &ObjectDef,
    objects: &mut Vec<PlannedCreate>,
    indexes: &mut Vec<String>,
    foreign_keys: &mut Vec<String>,
    baseline: &Side,
    other: &Side,
) {
    match object.kind {
        Kind::Table => {
            let Some(shape) = object.table.as_ref() else {
                return;
            };
            // SQLite 不能事后加外键，只能写在 CREATE TABLE 里，而且允许被引用表尚未存在。
            // PostgreSQL 与 MySQL 在 CREATE TABLE 里引用还没建的表会失败，所以外键一律留到全部建表之后。
            let include_fk = !blocked(baseline, other, Category::ForeignKey);
            let inline_fk = engine == Engine::Sqlite && include_fk;
            objects.push(plan_create(object, create_table(engine, target, &object.name, shape, inline_fk)));
            if include_fk && engine != Engine::Sqlite {
                foreign_keys.extend(shape.foreign_keys.iter().map(|key| add_foreign_key(engine, target, &object.name, key)));
            }
            if !blocked(baseline, other, Category::Index) {
                indexes.extend(shape.indexes.iter().filter(|index| !skip_index(index, shape)).map(|index| create_index(engine, target, &object.name, index)));
            }
        }
        _ => {
            if let Some(sql) = create_definition(engine, target, object) {
                objects.push(plan_create(object, sql));
            }
        }
    }
}

fn push_drop(engine: Engine, target: &Side, object: &ObjectDef, objects: &mut Vec<PlannedDrop>, foreign_keys: &mut Vec<String>, drop_owned_foreign_keys: bool) {
    if drop_owned_foreign_keys && object.kind == Kind::Table {
        if let Some(shape) = object.table.as_ref() {
            for key in &shape.foreign_keys {
                if key.name.trim().is_empty() {
                    continue;
                }
                foreign_keys.push(drop_foreign_key(engine, target, &object.name, key));
            }
        }
    }
    objects.push(plan_drop(engine, target, object));
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
    object_drops: &mut Vec<PlannedDrop>,
    create_object: &mut Vec<PlannedCreate>,
    notes: &mut Vec<String>,
    incomplete: &mut bool,
) {
    if base.kind != Kind::Table {
        if base.definition != other.definition || base.materialized != other.materialized {
            changed.push(item(base, "定义文本不同"));
            if allow_ddl && !blocked(baseline, target_side, Category::from_kind(base.kind)) {
                // 名称和签名都没变时，PostgreSQL 上的 DROP 会被仍引用它的对象拒绝。
                // 能证明输出列兼容的视图，以及返回类型没变的函数，直接替换。
                // 证明不了的视图先删依赖再 DROP，由后续步骤建回。
                if replaces_in_place(engine, base, other) {
                    if let Some(sql) = create_definition(engine, target, base) {
                        create_object.push(plan_replace(base, other, ensure_or_replace_kind(&sql, base.kind)));
                    }
                } else {
                    object_drops.push(plan_drop(engine, target, other));
                    if let Some(sql) = create_definition(engine, target, base) {
                        create_object.push(plan_create(base, sql));
                    }
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
    let fk_blocked = !fk_known || blocked(baseline, target_side, Category::ForeignKey);
    let with_fk = !fk_blocked;
    let index_blocked = blocked(baseline, target_side, Category::Index) || !left.indexes_read || !right.indexes_read;
    let sqlite_fk_only = engine == Engine::Sqlite && fk_diff && !column_diff && !pk_diff;
    let structural = engine == Engine::Sqlite && sqlite_needs_rebuild(left, right, with_fk);
    // 外键没读全时不能把空列表当成没有外键。SQLite 重建会在新表里丢掉它们。
    let rebuild_drops_unknown_fk = engine == Engine::Sqlite && fk_blocked && sqlite_needs_rebuild(left, right, false);
    // 仅外键不同时 SQLite 只能重建。索引没读全时重建会连带丢掉索引，因此不生成这类语句。
    let emit_alter = allow_ddl
        && !blocked(baseline, target_side, Category::Table)
        && !(structural && index_blocked)
        && !rebuild_drops_unknown_fk
        && (column_diff || pk_diff || (sqlite_fk_only && with_fk && !index_blocked));
    if rebuild_drops_unknown_fk && allow_ddl && !blocked(baseline, target_side, Category::Table) && (column_diff || pk_diff) {
        *incomplete = true;
        notes.push(format!("表 {} 的外键没有读全，不生成会丢掉外键的重建。", base.name));
    }
    if column_diff || pk_diff {
        changed.push(item(base, if column_diff { "列的名称、类型、可否为空或默认值不同" } else { "主键不同" }));
    }
    if emit_alter {
        alters.extend(alter_table(engine, target, baseline, &base.name, left, right, with_fk, pk_trusted));
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

#[derive(Clone)]
struct PlannedDrop {
    kind: Kind,
    name: String,
    key: String,
    statement: String,
    definition: String,
    /// 序列被表列拥有时是 `表` 或 `表\u{1}列`。触发器是所在的表或视图。
    relation: String,
    /// 必须先于这些对象删除。表是外键指向的同范围表，视图是定义里点到的其他视图。
    references: Vec<String>,
}

struct PlannedCreate {
    kind: Kind,
    name: String,
    key: String,
    statement: String,
    definition: String,
    relation: String,
    /// 对照上同名对象的现定义。空表示先删后建或全新对象，不是原地替换。
    current_definition: String,
}

fn plan_drop(engine: Engine, side: &Side, object: &ObjectDef) -> PlannedDrop {
    PlannedDrop {
        kind: object.kind,
        name: object.name.clone(),
        key: identity(object),
        statement: drop_object(engine, side, object),
        definition: object.definition.clone(),
        relation: object.relation.clone(),
        references: referenced_tables(engine, side, object),
    }
}

fn plan_create(object: &ObjectDef, statement: String) -> PlannedCreate {
    PlannedCreate {
        kind: object.kind,
        name: object.name.clone(),
        key: identity(object),
        statement,
        definition: object.definition.clone(),
        relation: object.relation.clone(),
        current_definition: String::new(),
    }
}

fn plan_replace(baseline: &ObjectDef, current: &ObjectDef, statement: String) -> PlannedCreate {
    let mut planned = plan_create(baseline, statement);
    planned.current_definition = current.definition.clone();
    planned
}

fn referenced_tables(engine: Engine, side: &Side, object: &ObjectDef) -> Vec<String> {
    let Some(shape) = object.table.as_ref() else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for key in &shape.foreign_keys {
        if key.ref_table.is_empty() {
            continue;
        }
        if engine == Engine::Postgres && !key.ref_schema.is_empty() && key.ref_schema != side.schema {
            continue;
        }
        names.push(key.ref_table.clone());
    }
    names
}

/// 触发器先于它所在的视图和表。视图按真实引用先删依赖方。表按外键先删引用方。
/// 枚举、域、序列和函数不在这里：列还在用它们时不能先删，新建又必须赶在 ALTER 或 CREATE TABLE 之前。
fn assemble_statements(
    engine: Engine,
    target: &Side,
    drop_fk: Vec<String>,
    drop_index: Vec<String>,
    drops: Vec<PlannedDrop>,
    alters: Vec<String>,
    creates: Vec<PlannedCreate>,
    create_index: Vec<String>,
    create_fk: Vec<String>,
    in_place: Vec<String>,
    detach: Vec<String>,
    restore: Vec<String>,
) -> Vec<String> {
    use std::collections::BTreeSet;
    let create_keys = creates.iter().map(|item| item.key.clone()).collect::<BTreeSet<_>>();
    let mut triggers = Vec::new();
    let mut views = Vec::new();
    let mut tables = Vec::new();
    let mut support_drops = Vec::new();
    for drop in drops {
        match drop.kind {
            Kind::Trigger => triggers.push(drop),
            Kind::View => views.push(drop),
            Kind::Table => tables.push(drop),
            _ => support_drops.push(drop),
        }
    }
    let by_name = |left: &PlannedDrop, right: &PlannedDrop| left.name.cmp(&right.name).then(left.statement.cmp(&right.statement));
    triggers.sort_by(by_name);
    // 引用即将删除的视图或表的例程必须先于该对象删除，不能留在后面的固定阶段。
    let dropped_view_names = views.iter().map(|view| view.name.clone()).collect::<BTreeSet<_>>();
    let dropped_table_names = tables.iter().map(|table| table.name.clone()).collect::<BTreeSet<_>>();
    let mut blocking_relations = dropped_view_names.clone();
    blocking_relations.extend(dropped_table_names.iter().cloned());
    let early_keys = routine_drop_keys(&support_drops, &blocking_relations);
    let mut early_routines = Vec::new();
    let mut late_support = Vec::new();
    for drop in support_drops {
        if early_keys.contains(&drop.key) {
            early_routines.push(drop);
        } else {
            late_support.push(drop);
        }
    }
    let support_drops = late_support;
    let dropped_tables = dropped_table_names;
    let mut changed_drops = Vec::new();
    let mut only_target_drops = Vec::new();
    for mut drop in support_drops {
        match sequence_fate(&drop, &dropped_tables, &alters, target) {
            SequenceFate::Omit => continue,
            SequenceFate::IfExists => {
                drop.statement = drop.statement.replacen("DROP SEQUENCE ", "DROP SEQUENCE IF EXISTS ", 1);
            }
            SequenceFate::Keep => {}
        }
        if create_keys.contains(&drop.key) {
            changed_drops.push(drop);
        } else {
            only_target_drops.push(drop);
        }
    }
    let changed_drops = order_support(changed_drops, true);
    let only_target_drops = order_support(only_target_drops, true);
    let mut changed_creates = Vec::new();
    let mut fresh_creates = Vec::new();
    let mut table_creates = Vec::new();
    let mut view_creates = Vec::new();
    let mut trigger_creates = Vec::new();
    let drop_keys = changed_drops.iter().chain(early_routines.iter()).map(|item| item.key.clone()).collect::<BTreeSet<_>>();
    let routine_drops = changed_drops
        .iter()
        .chain(only_target_drops.iter())
        .chain(early_routines.iter())
        .filter(|drop| matches!(drop.kind, Kind::Function | Kind::Procedure))
        .map(|drop| drop.name.clone())
        .collect::<Vec<_>>();
    let dropped_table_list = dropped_tables.iter().cloned().collect::<Vec<_>>();
    let dropped_view_list = dropped_view_names.iter().cloned().collect::<Vec<_>>();
    let mut early_views = Vec::new();
    for create in creates {
        match create.kind {
            Kind::Table => table_creates.push(create),
            Kind::View if is_early_view_replace(&create, &alters, &detach, &routine_drops, &dropped_table_list, &dropped_view_list) => {
                early_views.push(create)
            }
            Kind::View => view_creates.push(create),
            Kind::Trigger => trigger_creates.push(create),
            _ if drop_keys.contains(&create.key) => changed_creates.push(create),
            _ => fresh_creates.push(create),
        }
    }
    let by_create = |left: &PlannedCreate, right: &PlannedCreate| left.name.cmp(&right.name).then(left.statement.cmp(&right.statement));
    let changed_creates = order_support_creates(changed_creates);
    let fresh_creates = order_support_creates(fresh_creates);
    // 新建例程若要查询本次才创建的视图，就得跟视图一起排，不能全部放在视图前面。
    let created_view_names = view_creates.iter().map(|view| view.name.clone()).collect::<BTreeSet<_>>();
    let waiting_keys = create_keys_depending_on_views(&fresh_creates, &changed_creates, &created_view_names);
    let (fresh_creates, mut delayed_routines) = split_waiting(fresh_creates, &waiting_keys);
    let (changed_creates, delayed_more) = split_waiting(changed_creates, &waiting_keys);
    delayed_routines.extend(delayed_more);
    // 提前替换已经写成基准定义。再 DROP 会把仍要保留的视图删掉，而这条创建不会再出现。
    let early_view_keys = early_views.iter().map(|create| create.key.clone()).collect::<BTreeSet<_>>();
    views.retain(|drop| !early_view_keys.contains(&drop.key));
    let changed_names = changed_creates.iter().map(|item| item.name.clone()).collect::<Vec<_>>();
    let fresh_names = fresh_creates.iter().map(|item| item.name.clone()).collect::<Vec<_>>();
    let delayed_names = delayed_routines.iter().map(|item| item.name.clone()).collect::<Vec<_>>();
    // 改到新类型的 ALTER 会提到新名字。新对象还没建时不能先跑；旧类型还被列用着时也不能先 DROP。
    let (freeing_alters, before_drop, after_recreate) = partition_alters(&alters, &fresh_names, &changed_names);
    table_creates.sort_by(by_create);
    trigger_creates.sort_by(by_create);
    let (freeing_alters, late_freeing) = split_mentions(freeing_alters, &delayed_names);
    let (before_drop, late_before) = split_mentions(before_drop, &delayed_names);
    let (after_recreate, late_after) = split_mentions(after_recreate, &delayed_names);
    let (restore_now, late_restore) = split_mentions(restore, &delayed_names);
    // 列默认值的语句里没有函数名。引用被提前删除的例程时，先去掉默认值，否则 DROP FUNCTION 会被拒绝。
    let pull_defaults = !early_routines.is_empty();
    let (detach_defaults, detach) = take_default_drops_if(pull_defaults, detach);
    let (alter_defaults, freeing_alters) = take_default_drops_if(pull_defaults, freeing_alters);
    let (before_defaults, before_drop) = take_default_drops_if(pull_defaults, before_drop);
    let (after_defaults, after_recreate) = take_default_drops_if(pull_defaults, after_recreate);
    let (late_freeing_defaults, late_freeing) = take_default_drops_if(pull_defaults, late_freeing);
    let (late_before_defaults, late_before) = take_default_drops_if(pull_defaults, late_before);
    let (late_after_defaults, late_after) = take_default_drops_if(pull_defaults, late_after);
    let mut early_detach = detach_defaults;
    early_detach.extend(alter_defaults);
    early_detach.extend(before_defaults);
    early_detach.extend(after_defaults);
    early_detach.extend(late_freeing_defaults);
    early_detach.extend(late_before_defaults);
    early_detach.extend(late_after_defaults);
    let mut statements = Vec::new();
    statements.extend(drop_fk);
    statements.extend(drop_index);
    statements.extend(triggers.into_iter().map(|drop| drop.statement));
    // 还引用即将删掉的函数时，先换成不再引用它的定义，后面的 DROP FUNCTION 才过得去。
    statements.extend(order_view_creates(early_views).into_iter().map(|create| create.statement));
    statements.extend(early_detach);
    statements.extend(order_view_and_routines(engine, target, views, early_routines));
    statements.extend(order_dependents_first(tables).into_iter().map(|drop| drop.statement));
    statements.extend(in_place);
    statements.extend(detach);
    statements.extend(freeing_alters);
    statements.extend(fresh_creates.into_iter().map(|create| create.statement));
    statements.extend(before_drop);
    statements.extend(changed_drops.into_iter().flat_map(|drop| expand_sequence_drop(engine, target, drop)));
    statements.extend(changed_creates.into_iter().map(|create| create.statement));
    statements.extend(restore_now);
    statements.extend(after_recreate);
    statements.extend(only_target_drops.into_iter().flat_map(|drop| expand_sequence_drop(engine, target, drop)));
    statements.extend(table_creates.into_iter().map(|create| create.statement));
    statements.extend(order_view_and_routine_creates(engine, target, view_creates, delayed_routines));
    statements.extend(late_freeing);
    statements.extend(late_before);
    statements.extend(late_after);
    statements.extend(late_restore);
    statements.extend(trigger_creates.into_iter().map(|create| create.statement));
    statements.extend(create_index);
    statements.extend(create_fk);
    statements
}

fn routine_drop_keys(items: &[PlannedDrop], view_names: &std::collections::BTreeSet<String>) -> std::collections::BTreeSet<String> {
    let routines = items
        .iter()
        .filter(|item| matches!(item.kind, Kind::Function | Kind::Procedure))
        .map(|item| (item.key.clone(), item.name.clone(), item.definition.clone()))
        .collect::<Vec<_>>();
    blocking_routine_keys(&routines, view_names)
}

fn create_keys_depending_on_views(
    fresh: &[PlannedCreate],
    changed: &[PlannedCreate],
    view_names: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeSet<String> {
    let routines = fresh
        .iter()
        .chain(changed.iter())
        .filter(|item| matches!(item.kind, Kind::Function | Kind::Procedure))
        .map(|item| (item.key.clone(), item.name.clone(), item.definition.clone()))
        .collect::<Vec<_>>();
    blocking_routine_keys(&routines, view_names)
}

/// 直接或间接查询这些视图的例程。视图引用例程不算，那种例程仍留在视图之后删除、之前创建。
fn blocking_routine_keys(routines: &[(String, String, String)], view_names: &std::collections::BTreeSet<String>) -> std::collections::BTreeSet<String> {
    use std::collections::BTreeSet;
    let mut blocking = BTreeSet::new();
    let mut progressed = true;
    while progressed {
        progressed = false;
        for (key, _, definition) in routines {
            if blocking.contains(key) {
                continue;
            }
            let on_view = referenced_relations(definition).iter().any(|found| view_names.iter().any(|view| found.eq_ignore_ascii_case(view)));
            let on_routine = routines.iter().any(|(other_key, other_name, _)| blocking.contains(other_key) && other_key != key && calls_routine(definition, other_name));
            if on_view || on_routine {
                blocking.insert(key.clone());
                progressed = true;
            }
        }
    }
    blocking
}

fn split_waiting(items: Vec<PlannedCreate>, waiting: &std::collections::BTreeSet<String>) -> (Vec<PlannedCreate>, Vec<PlannedCreate>) {
    let mut kept = Vec::new();
    let mut delayed = Vec::new();
    for item in items {
        if waiting.contains(&item.key) {
            delayed.push(item);
        } else {
            kept.push(item);
        }
    }
    (kept, delayed)
}

fn split_mentions(items: Vec<String>, names: &[String]) -> (Vec<String>, Vec<String>) {
    let mut kept = Vec::new();
    let mut moved = Vec::new();
    for item in items {
        if names.iter().any(|name| statement_mentions(&item, name)) {
            moved.push(item);
        } else {
            kept.push(item);
        }
    }
    (kept, moved)
}

fn take_default_drops_if(enabled: bool, items: Vec<String>) -> (Vec<String>, Vec<String>) {
    if !enabled {
        return (Vec::new(), items);
    }
    let mut early = Vec::new();
    let mut rest = Vec::new();
    for item in items {
        if item.to_ascii_uppercase().contains("DROP DEFAULT") {
            early.push(item);
        } else {
            rest.push(item);
        }
    }
    (early, rest)
}

fn order_view_and_routines(engine: Engine, side: &Side, mut views: Vec<PlannedDrop>, routines: Vec<PlannedDrop>) -> Vec<String> {
    views.extend(routines);
    link_view_routine_references(&mut views);
    order_linked(engine, side, views, true)
}

fn order_view_and_routine_creates(engine: Engine, side: &Side, views: Vec<PlannedCreate>, routines: Vec<PlannedCreate>) -> Vec<String> {
    let mut items = views.into_iter().chain(routines).map(drop_from_create).collect::<Vec<_>>();
    link_view_routine_references(&mut items);
    order_linked(engine, side, items, false)
}

fn drop_from_create(create: PlannedCreate) -> PlannedDrop {
    PlannedDrop {
        kind: create.kind,
        name: create.name,
        key: create.key,
        statement: create.statement,
        definition: create.definition,
        relation: create.relation,
        references: Vec::new(),
    }
}

/// 视图之间只用 FROM/JOIN。例程查询视图、视图调用例程都记在同一张图里，避免全部函数固定在全部视图的一侧。
fn link_view_routine_references(items: &mut [PlannedDrop]) {
    let views = items.iter().filter(|item| item.kind == Kind::View).map(|item| (item.key.clone(), item.name.clone())).collect::<Vec<_>>();
    let routines = items
        .iter()
        .filter(|item| matches!(item.kind, Kind::Function | Kind::Procedure))
        .map(|item| (item.key.clone(), item.name.clone()))
        .collect::<Vec<_>>();
    for item in items.iter_mut() {
        if item.kind == Kind::View {
            let mut refs = referenced_relations(&item.definition)
                .into_iter()
                .filter_map(|found| views.iter().find(|(_, name)| found.eq_ignore_ascii_case(name)).map(|(key, _)| key.clone()))
                .filter(|key| key != &item.key)
                .collect::<std::collections::BTreeSet<_>>();
            for (key, name) in &routines {
                // 列名、别名和同名标识不是调用。只有 name( 才让视图依赖例程。
                if calls_routine(&item.definition, name) {
                    refs.insert(key.clone());
                }
            }
            item.references = refs.into_iter().collect();
        } else if matches!(item.kind, Kind::Function | Kind::Procedure) {
            let mut refs = std::collections::BTreeSet::new();
            for (key, name) in &routines {
                if key != &item.key && calls_routine(&item.definition, name) {
                    refs.insert(key.clone());
                }
            }
            for (key, name) in &views {
                if referenced_relations(&item.definition).iter().any(|found| found.eq_ignore_ascii_case(name)) {
                    refs.insert(key.clone());
                }
            }
            item.references = refs.into_iter().collect();
        }
    }
}

/// 没有视图和例程互相引用的环时，沿用标识拓扑。有环则先把例程换成不引用对方的正文，再删或建真正对象，最后写回原定义。
fn order_linked(engine: Engine, side: &Side, items: Vec<PlannedDrop>, dependents_first: bool) -> Vec<String> {
    if engine == Engine::Postgres && mixed_view_routine_cycle(&items) {
        emit_breaking_cycles(engine, side, items, dependents_first)
    } else {
        order_planned(items, dependents_first).into_iter().map(|item| item.statement).collect()
    }
}

fn mixed_view_routine_cycle(items: &[PlannedDrop]) -> bool {
    component_keys(items).into_iter().any(|component| {
        component.len() > 1
            && component.iter().any(|key| item_kind(items, key) == Some(Kind::View))
            && component.iter().any(|key| matches!(item_kind(items, key), Some(Kind::Function | Kind::Procedure)))
    })
}

fn item_kind(items: &[PlannedDrop], key: &str) -> Option<Kind> {
    items.iter().find(|item| item.key == key).map(|item| item.kind)
}

fn dependency_map(items: &[PlannedDrop]) -> std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
    let mut map = std::collections::BTreeMap::new();
    for item in items {
        let entry = map.entry(item.key.clone()).or_insert_with(std::collections::BTreeSet::new);
        for referenced in &item.references {
            if referenced != &item.key && items.iter().any(|candidate| candidate.key == *referenced) {
                entry.insert(referenced.clone());
            }
        }
    }
    map
}

fn component_keys(items: &[PlannedDrop]) -> Vec<Vec<String>> {
    use std::collections::{BTreeMap, BTreeSet};
    let deps = dependency_map(items);
    let keys = deps.keys().cloned().collect::<Vec<_>>();
    let mut reverse: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for key in &keys {
        reverse.entry(key.clone()).or_default();
        for dependency in deps.get(key).into_iter().flatten() {
            reverse.entry(dependency.clone()).or_default().push(key.clone());
        }
    }
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    for key in &keys {
        visit_forward(key, &deps, &mut seen, &mut order);
    }
    seen.clear();
    let mut components = Vec::new();
    for key in order.iter().rev() {
        if seen.contains(key) {
            continue;
        }
        let mut component = Vec::new();
        visit_reverse(key, &reverse, &mut seen, &mut component);
        component.sort();
        components.push(component);
    }
    components
}

fn visit_forward(key: &str, deps: &std::collections::BTreeMap<String, std::collections::BTreeSet<String>>, seen: &mut std::collections::BTreeSet<String>, order: &mut Vec<String>) {
    if !seen.insert(key.to_owned()) {
        return;
    }
    for next in deps.get(key).into_iter().flatten() {
        visit_forward(next, deps, seen, order);
    }
    order.push(key.to_owned());
}

fn visit_reverse(key: &str, reverse: &std::collections::BTreeMap<String, Vec<String>>, seen: &mut std::collections::BTreeSet<String>, component: &mut Vec<String>) {
    if !seen.insert(key.to_owned()) {
        return;
    }
    component.push(key.to_owned());
    for next in reverse.get(key).into_iter().flatten() {
        visit_reverse(next, reverse, seen, component);
    }
}

fn emit_breaking_cycles(engine: Engine, side: &Side, items: Vec<PlannedDrop>, dependents_first: bool) -> Vec<String> {
    use std::collections::{BTreeMap, BTreeSet};
    let deps = dependency_map(&items);
    let mut by_key: BTreeMap<String, Vec<PlannedDrop>> = BTreeMap::new();
    for item in items {
        by_key.entry(item.key.clone()).or_default().push(item);
    }
    let mut components = component_keys(&by_key.values().flatten().cloned().collect::<Vec<_>>());
    components.sort_by(|left, right| left[0].cmp(&right[0]));
    let key_component = components
        .iter()
        .enumerate()
        .flat_map(|(index, component)| component.iter().map(move |key| (key.clone(), index)))
        .collect::<BTreeMap<_, _>>();
    let mut indegree = vec![0usize; components.len()];
    let mut edges = vec![BTreeSet::new(); components.len()];
    for (key, references) in &deps {
        let Some(&from_component) = key_component.get(key) else {
            continue;
        };
        for referenced in references {
            let Some(&to_component) = key_component.get(referenced) else {
                continue;
            };
            if from_component == to_component {
                continue;
            }
            let (source, dest) = if dependents_first { (from_component, to_component) } else { (to_component, from_component) };
            if edges[source].insert(dest) {
                indegree[dest] += 1;
            }
        }
    }
    let mut ready = indegree.iter().enumerate().filter(|(_, degree)| **degree == 0).map(|(index, _)| index).collect::<BTreeSet<_>>();
    let mut ordered = Vec::new();
    while let Some(index) = ready.iter().next().cloned() {
        ready.remove(&index);
        ordered.push(index);
        for next in edges[index].clone() {
            indegree[next] -= 1;
            if indegree[next] == 0 {
                ready.insert(next);
            }
        }
    }
    for index in 0..components.len() {
        if !ordered.contains(&index) {
            ordered.push(index);
        }
    }
    let mut statements = Vec::new();
    for index in ordered {
        let component = &components[index];
        let mixed = component.iter().any(|key| by_key.get(key).and_then(|group| group.first()).is_some_and(|item| item.kind == Kind::View))
            && component.iter().any(|key| by_key.get(key).and_then(|group| group.first()).is_some_and(|item| matches!(item.kind, Kind::Function | Kind::Procedure)));
        if mixed && component.len() > 1 {
            statements.extend(break_view_routine_cycle(engine, side, component, &by_key, dependents_first));
        } else {
            for key in component {
                if let Some(group) = by_key.get(key) {
                    statements.extend(group.iter().map(|item| item.statement.clone()));
                }
            }
        }
    }
    statements
}

fn break_view_routine_cycle(
    engine: Engine,
    side: &Side,
    component: &[String],
    by_key: &std::collections::BTreeMap<String, Vec<PlannedDrop>>,
    dependents_first: bool,
) -> Vec<String> {
    let mut routines = Vec::new();
    let mut views = Vec::new();
    for key in component {
        for item in by_key.get(key).into_iter().flatten() {
            if matches!(item.kind, Kind::Function | Kind::Procedure) {
                routines.push(item.clone());
            } else if item.kind == Kind::View {
                views.push(item.clone());
            }
        }
    }
    routines.sort_by(|left, right| left.name.cmp(&right.name).then(left.key.cmp(&right.key)));
    let view_statements = order_planned(views, dependents_first).into_iter().map(|item| item.statement);
    let mut statements = Vec::new();
    if dependents_first {
        for routine in &routines {
            if let Some(stub) = stub_existing_routine(engine, side, routine) {
                statements.push(stub);
            }
        }
        statements.extend(view_statements);
        statements.extend(routines.iter().map(|routine| routine.statement.clone()));
    } else {
        for routine in &routines {
            if let Some(stub) = stub_new_routine(&routine.statement) {
                statements.push(stub);
            }
        }
        statements.extend(view_statements);
        statements.extend(routines.iter().map(|routine| ensure_or_replace(&routine.statement)));
    }
    statements
}

fn stub_existing_routine(engine: Engine, side: &Side, routine: &PlannedDrop) -> Option<String> {
    let keyword = if routine.kind == Kind::Procedure { "PROCEDURE" } else { "FUNCTION" };
    let source = routine.definition.trim().trim_end_matches(';').trim();
    if source.len() < 6 || !source[..6].eq_ignore_ascii_case("CREATE") {
        return None;
    }
    let qualified = replace_name_after(source, keyword, &qualify(engine, side, &routine.name))?;
    Some(ensure_or_replace(&stub_routine(&qualified)?))
}

fn stub_new_routine(statement: &str) -> Option<String> {
    let stub = stub_routine(statement)?;
    if has_or_replace(statement) { Some(ensure_or_replace(&stub)) } else { Some(stub) }
}

fn has_or_replace(sql: &str) -> bool {
    let trimmed = sql.trim_start();
    if trimmed.len() < 6 || !trimmed[..6].eq_ignore_ascii_case("CREATE") {
        return false;
    }
    let rest = trimmed[6..].trim_start();
    rest.len() >= 10 && rest[..10].eq_ignore_ascii_case("OR REPLACE")
}

/// 保留签名和返回类型，正文不再引用视图或其它例程。
fn stub_routine(sql: &str) -> Option<String> {
    let function_at = find_keyword_end(sql, "FUNCTION");
    let procedure_at = find_keyword_end(sql, "PROCEDURE");
    let (keyword_end, procedure) = match (function_at, procedure_at) {
        (Some(function), Some(procedure)) if procedure < function => (procedure, true),
        (Some(function), _) => (function, false),
        (_, Some(procedure)) => (procedure, true),
        _ => return None,
    };
    let name_end = qualified_name_end(sql, skip_gap(sql, keyword_end))?;
    let mut index = skip_gap(sql, name_end);
    if sql[index..].starts_with('(') {
        index = skip_raw_paren(sql, index)?;
    }
    index = skip_gap(sql, index);
    let mut returns = String::new();
    if !procedure && starts_with_word(sql, index, "RETURNS") {
        index = skip_gap(sql, skip_word(sql, index));
        let start = index;
        index = skip_returns_type(sql, index);
        returns = sql[start..index].trim().to_owned();
    }
    let header = sql[..index].trim_end();
    Some(stub_with_header(header, procedure, &returns))
}

fn stub_with_header(header: &str, procedure: bool, returns: &str) -> String {
    if procedure {
        return format!("{header} LANGUAGE plpgsql AS $$ BEGIN NULL; END $$");
    }
    let upper = returns.to_ascii_uppercase();
    if returns.is_empty() || upper.contains("SETOF") || upper.contains("TABLE") || upper == "VOID" {
        format!("{header} LANGUAGE plpgsql AS $$ BEGIN RETURN; END $$")
    } else {
        format!("{header} LANGUAGE sql AS $$ SELECT NULL::{returns} $$")
    }
}

fn skip_returns_type(sql: &str, mut index: usize) -> usize {
    let mut depth = 0i32;
    while index < sql.len() {
        if let Some(next) = skip_delimited(sql, index) {
            index = next;
            continue;
        }
        let ch = sql[index..].chars().next().unwrap();
        if ch == '(' {
            depth += 1;
            index += 1;
            continue;
        }
        if ch == ')' {
            depth = depth.saturating_sub(1);
            index += 1;
            continue;
        }
        if depth == 0 && is_ident_start(ch) {
            let end = ident_continue(sql, index);
            if is_returns_stop(&sql[index..end]) {
                return index;
            }
            index = end;
            continue;
        }
        index += ch.len_utf8();
    }
    index
}

fn is_returns_stop(word: &str) -> bool {
    matches!(
        word.to_ascii_uppercase().as_str(),
        "LANGUAGE" | "AS" | "WINDOW" | "IMMUTABLE" | "STABLE" | "VOLATILE" | "LEAKPROOF" | "CALLED" | "STRICT" | "COST" | "ROWS" | "PARALLEL" | "SECURITY" | "SET" | "TRANSFORM" | "NOT"
    )
}

fn skip_raw_paren(sql: &str, open: usize) -> Option<usize> {
    if !sql.get(open..)?.starts_with('(') {
        return None;
    }
    let mut depth = 0i32;
    let mut index = open;
    while index < sql.len() {
        if let Some(next) = skip_delimited(sql, index) {
            index = next;
            continue;
        }
        let ch = sql[index..].chars().next()?;
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            index += 1;
            if depth == 0 {
                return Some(index);
            }
            continue;
        }
        index += ch.len_utf8();
    }
    None
}

fn calls_routine(sql: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let tokens = sql_tokens(sql);
    let mut index = 0;
    while index < tokens.len() {
        if is_word(&tokens[index], "CREATE") {
            let next = skip_created_signature(&tokens, index);
            if next > index {
                index = next;
                continue;
            }
        }
        if is_ident_token(&tokens[index]) {
            let (found, next) = qualified_relation(&tokens, index);
            if next > index && next < tokens.len() && tokens[next].text == "(" && found.eq_ignore_ascii_case(name) {
                return true;
            }
            if next > index {
                index = next;
                continue;
            }
        }
        index += 1;
    }
    false
}

fn skip_created_signature(tokens: &[SqlToken], create_at: usize) -> usize {
    let mut index = create_at + 1;
    if index < tokens.len() && is_word(&tokens[index], "OR") {
        index += 1;
        if index < tokens.len() && is_word(&tokens[index], "REPLACE") {
            index += 1;
        }
    }
    let mut guard = 0;
    while index < tokens.len() && guard < 6 {
        if matches_created_keyword(&tokens[index]) {
            break;
        }
        if is_word(&tokens[index], "TEMP") || is_word(&tokens[index], "TEMPORARY") {
            index += 1;
            guard += 1;
            continue;
        }
        return create_at + 1;
    }
    if index >= tokens.len() {
        return create_at + 1;
    }
    if is_word(&tokens[index], "MATERIALIZED") {
        index += 1;
    }
    if index < tokens.len() && matches_created_keyword(&tokens[index]) && !is_word(&tokens[index], "MATERIALIZED") {
        index += 1;
    } else if index >= tokens.len() || !is_word(&tokens[index], "VIEW") {
        return create_at + 1;
    }
    if index >= tokens.len() || !is_ident_token(&tokens[index]) {
        return index;
    }
    let (_, next) = qualified_relation(tokens, index);
    index = if next > index { next } else { index + 1 };
    if index < tokens.len() && tokens[index].text == "(" {
        index = matching_paren(tokens, index).saturating_add(1);
    }
    index
}

fn matches_created_keyword(token: &SqlToken) -> bool {
    is_word(token, "VIEW") || is_word(token, "FUNCTION") || is_word(token, "PROCEDURE") || is_word(token, "TRIGGER") || is_word(token, "MATERIALIZED")
}

fn order_planned(items: Vec<PlannedDrop>, dependents_first: bool) -> Vec<PlannedDrop> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut by_key: BTreeMap<String, Vec<PlannedDrop>> = BTreeMap::new();
    for item in items {
        by_key.entry(item.key.clone()).or_default().push(item);
    }
    let mut forward: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut indegree: BTreeMap<String, usize> = BTreeMap::new();
    for key in by_key.keys() {
        forward.insert(key.clone(), BTreeSet::new());
        indegree.insert(key.clone(), 0);
    }
    for group in by_key.values() {
        for item in group {
            for referenced in &item.references {
                if referenced == &item.key || !by_key.contains_key(referenced) {
                    continue;
                }
                let (from, to) = if dependents_first { (item.key.clone(), referenced.clone()) } else { (referenced.clone(), item.key.clone()) };
                if forward.get_mut(&from).expect("from").insert(to.clone()) {
                    *indegree.get_mut(&to).expect("to") += 1;
                }
            }
        }
    }
    let mut ready: BTreeSet<String> = indegree.iter().filter(|(_, degree)| **degree == 0).map(|(key, _)| key.clone()).collect();
    let mut ordered = Vec::new();
    while let Some(key) = ready.iter().next().cloned() {
        ready.remove(&key);
        ordered.push(key.clone());
        for next in forward.get(&key).into_iter().flatten() {
            let degree = indegree.get_mut(next).expect("next");
            *degree -= 1;
            if *degree == 0 {
                ready.insert(next.clone());
            }
        }
    }
    let mut cyclic = indegree
        .iter()
        .filter(|(key, degree)| **degree > 0 && !ordered.iter().any(|item| item == *key))
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    cyclic.sort();
    ordered.extend(cyclic);
    let mut result = Vec::new();
    for key in ordered {
        if let Some(group) = by_key.remove(&key) {
            result.extend(group);
        }
    }
    result
}

fn order_support(mut items: Vec<PlannedDrop>, dependents_first: bool) -> Vec<PlannedDrop> {
    attach_definition_references(&mut items);
    if dependents_first {
        order_ranked(items, true, support_drop_rank)
    } else {
        order_ranked(items, false, support_create_rank)
    }
}

fn order_support_creates(items: Vec<PlannedCreate>) -> Vec<PlannedCreate> {
    let planned = items
        .iter()
        .map(|item| PlannedDrop {
            kind: item.kind,
            name: item.name.clone(),
            key: item.key.clone(),
            statement: item.statement.clone(),
            definition: item.definition.clone(),
            relation: item.relation.clone(),
            references: Vec::new(),
        })
        .collect::<Vec<_>>();
    let ordered = order_support(planned, false);
    let mut by_key = items.into_iter().map(|item| (item.key.clone(), item)).collect::<std::collections::BTreeMap<_, _>>();
    ordered.into_iter().filter_map(|drop| by_key.remove(&drop.key)).collect()
}

/// 定义文本里点到的同批名字是依赖。美元引号里的函数体也要扫，否则调用顺序只能靠名称。
fn attach_definition_references(items: &mut [PlannedDrop]) {
    let names = items.iter().map(|item| item.name.clone()).collect::<std::collections::BTreeSet<_>>();
    for item in items.iter_mut() {
        let mut refs = std::collections::BTreeSet::new();
        for token in sql_tokens(&item.definition) {
            if !is_ident_token(&token) {
                continue;
            }
            if let Some(known) = names.iter().find(|name| token.text.eq_ignore_ascii_case(name) && !name.eq_ignore_ascii_case(&item.name)) {
                refs.insert(known.clone());
            }
        }
        item.references = refs.into_iter().collect();
    }
}

fn support_drop_rank(kind: Kind) -> u8 {
    match kind {
        Kind::Procedure => 0,
        Kind::Function => 1,
        Kind::Domain => 2,
        Kind::Enum => 3,
        Kind::Sequence => 4,
        _ => 9,
    }
}

fn support_create_rank(kind: Kind) -> u8 {
    match kind {
        Kind::Enum => 0,
        Kind::Domain => 1,
        Kind::Sequence => 2,
        Kind::Function => 3,
        Kind::Procedure => 4,
        _ => 9,
    }
}

fn order_ranked(items: Vec<PlannedDrop>, dependents_first: bool, rank: fn(Kind) -> u8) -> Vec<PlannedDrop> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut by_name: BTreeMap<String, Vec<PlannedDrop>> = BTreeMap::new();
    let mut kind_of: BTreeMap<String, Kind> = BTreeMap::new();
    for item in items {
        kind_of.insert(item.name.clone(), item.kind);
        by_name.entry(item.name.clone()).or_default().push(item);
    }
    let mut forward: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut indegree: BTreeMap<String, usize> = BTreeMap::new();
    for name in by_name.keys() {
        forward.insert(name.clone(), BTreeSet::new());
        indegree.insert(name.clone(), 0);
    }
    for group in by_name.values() {
        for item in group {
            for referenced in &item.references {
                if referenced.eq_ignore_ascii_case(&item.name) || !by_name.contains_key(referenced) {
                    continue;
                }
                let (from, to) = if dependents_first { (item.name.clone(), referenced.clone()) } else { (referenced.clone(), item.name.clone()) };
                if forward.get_mut(&from).expect("from").insert(to.clone()) {
                    *indegree.get_mut(&to).expect("to") += 1;
                }
            }
        }
    }
    let key = |name: &str| (rank(kind_of[name]), name.to_owned());
    let mut ready: BTreeSet<(u8, String)> = indegree.iter().filter(|(_, degree)| **degree == 0).map(|(name, _)| key(name)).collect();
    let mut ordered = Vec::new();
    while let Some((_, name)) = ready.iter().next().cloned() {
        ready.remove(&key(&name));
        ordered.push(name.clone());
        for next in forward.get(&name).into_iter().flatten() {
            let degree = indegree.get_mut(next).expect("next");
            *degree -= 1;
            if *degree == 0 {
                ready.insert(key(next));
            }
        }
    }
    let mut cyclic = indegree
        .iter()
        .filter(|(name, degree)| **degree > 0 && !ordered.iter().any(|item| item == *name))
        .map(|(name, _)| key(name))
        .collect::<Vec<_>>();
    cyclic.sort();
    ordered.extend(cyclic.into_iter().map(|(_, name)| name));
    let mut result = Vec::new();
    for name in ordered {
        if let Some(group) = by_name.remove(&name) {
            result.extend(group);
        }
    }
    result
}

/// freeing：不依赖本次新建的对象，可以在删除之前执行。
/// before_drop：要用到只新增、不用先删的对象，并且必须赶在旧对象的 DROP 之前。
/// after_recreate：要用到先删后建的同名对象，只能等它建回来。
fn partition_alters(alters: &[String], fresh_names: &[String], changed_names: &[String]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut freeing = Vec::new();
    let mut before_drop = Vec::new();
    let mut after_recreate = Vec::new();
    for statement in alters {
        let mentions = |name: &String| statement_mentions(statement, name) || default_references_sequence(statement, name);
        if changed_names.iter().any(mentions) {
            after_recreate.push(statement.clone());
        } else if fresh_names.iter().any(mentions) {
            before_drop.push(statement.clone());
        } else {
            freeing.push(statement.clone());
        }
    }
    (freeing, before_drop, after_recreate)
}

enum SequenceFate {
    /// 明确属于被删表或被删列。DROP TABLE / DROP COLUMN 会带走它，再删会因对象已不存在而失败。
    Omit,
    /// 只看到 nextval，不能确定是 serial 还是普通引用。放在删表之后，用 IF EXISTS 两种都成立。
    IfExists,
    Keep,
}

fn sequence_fate(seq: &PlannedDrop, dropped_tables: &std::collections::BTreeSet<String>, alters: &[String], target: &Side) -> SequenceFate {
    if seq.kind != Kind::Sequence {
        return SequenceFate::Keep;
    }
    let (owner_table, owner_column) = explicit_sequence_owner(seq);
    if let Some(table) = owner_table.as_deref() {
        if dropped_tables.contains(table) {
            return SequenceFate::Omit;
        }
        if let Some(column) = owner_column.as_deref() {
            if column_is_dropped(alters, table, column) {
                return SequenceFate::Omit;
            }
        }
    }
    let mut referenced_by_removed_column = false;
    for object in target.objects.iter().filter(|object| object.kind == Kind::Table) {
        let Some(shape) = object.table.as_ref() else {
            continue;
        };
        for column in &shape.columns {
            if !default_references_sequence(&column.default_sql, &seq.name) {
                continue;
            }
            if dropped_tables.contains(&object.name) || column_is_dropped(alters, &object.name, &column.name) {
                referenced_by_removed_column = true;
            }
        }
    }
    if referenced_by_removed_column && owner_table.is_none() {
        SequenceFate::IfExists
    } else {
        SequenceFate::Keep
    }
}

fn explicit_sequence_owner(seq: &PlannedDrop) -> (Option<String>, Option<String>) {
    let relation = seq.relation.trim();
    if !relation.is_empty() {
        let mut parts = relation.split('\u{1}');
        return (parts.next().map(str::to_owned), parts.next().map(str::to_owned));
    }
    owned_by_clause(&seq.definition)
}

fn owned_by_clause(definition: &str) -> (Option<String>, Option<String>) {
    let tokens = sql_tokens(definition);
    let mut index = 0;
    while index + 1 < tokens.len() {
        if is_word(&tokens[index], "OWNED") && is_word(&tokens[index + 1], "BY") {
            let parts = qualified_parts(&tokens[index + 2..]);
            if parts.first().is_some_and(|part| part.eq_ignore_ascii_case("NONE")) || parts.is_empty() {
                return (None, None);
            }
            return match parts.len() {
                1 => (Some(parts[0].clone()), None),
                2 => (Some(parts[0].clone()), Some(parts[1].clone())),
                _ => (Some(parts[parts.len() - 2].clone()), Some(parts[parts.len() - 1].clone())),
            };
        }
        index += 1;
    }
    (None, None)
}

fn qualified_parts(tokens: &[SqlToken]) -> Vec<String> {
    if tokens.is_empty() || !is_ident_token(&tokens[0]) {
        return Vec::new();
    }
    let mut parts = vec![tokens[0].text.clone()];
    let mut index = 1;
    while index + 1 < tokens.len() && tokens[index].text == "." && is_ident_token(&tokens[index + 1]) {
        parts.push(tokens[index + 1].text.clone());
        index += 2;
    }
    parts
}

fn column_is_dropped(alters: &[String], table: &str, column: &str) -> bool {
    alters.iter().any(|sql| sql.to_ascii_uppercase().contains("DROP COLUMN") && statement_mentions(sql, table) && statement_mentions(sql, column))
}

fn expand_sequence_drop(engine: Engine, side: &Side, drop: PlannedDrop) -> Vec<String> {
    let mut statements = Vec::new();
    if engine == Engine::Postgres && drop.kind == Kind::Sequence && explicit_sequence_owner(&drop).0.is_some() {
        statements.push(format!("ALTER SEQUENCE {} OWNED BY NONE", qualify(engine, side, &drop.name)));
    }
    statements.push(drop.statement);
    statements
}

fn default_references_sequence(default_sql: &str, sequence: &str) -> bool {
    if sequence.is_empty() || !default_sql.to_ascii_lowercase().contains("nextval") {
        return false;
    }
    string_literals(default_sql).into_iter().any(|literal| {
        literal
            .split('.')
            .last()
            .unwrap_or(literal.as_str())
            .trim_matches('"')
            .eq_ignore_ascii_case(sequence)
    }) || statement_mentions(default_sql, sequence)
}

fn string_literals(sql: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut index = 0;
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
        if ch == '\'' {
            if let Some(end) = skip_quoted(sql, index, '\'') {
                literals.push(unquote(&sql[index..end]));
                index = end;
                continue;
            }
        }
        if ch == '$' {
            if let Some(end) = skip_dollar(sql, index) {
                index = end;
                continue;
            }
        }
        index += ch.len_utf8();
    }
    literals
}

fn order_view_creates(views: Vec<PlannedCreate>) -> Vec<PlannedCreate> {
    let mut planned = views
        .iter()
        .map(|view| PlannedDrop {
            kind: Kind::View,
            name: view.name.clone(),
            key: view.key.clone(),
            statement: view.statement.clone(),
            definition: view.definition.clone(),
            relation: view.relation.clone(),
            references: Vec::new(),
        })
        .collect::<Vec<_>>();
    attach_view_dependencies(&mut planned);
    let ordered = order_dependencies_first(planned);
    let mut by_key = views.into_iter().map(|view| (view.key.clone(), view)).collect::<std::collections::BTreeMap<_, _>>();
    ordered.into_iter().filter_map(|drop| by_key.remove(&drop.key)).collect()
}

fn attach_view_dependencies(views: &mut [PlannedDrop]) {
    let names = views.iter().map(|view| view.name.clone()).collect::<std::collections::BTreeSet<_>>();
    for view in views.iter_mut() {
        view.references = referenced_relations(&view.definition)
            .into_iter()
            .filter_map(|found| names.iter().find(|name| found.eq_ignore_ascii_case(name)).cloned())
            .filter(|name| !name.eq_ignore_ascii_case(&view.name))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
    }
}

/// 关系名来自 FROM / JOIN，以及 INSERT、UPDATE 的目标。字符串、注释、别名和 WITH 里的 CTE 名不算引用。
fn referenced_relations(sql: &str) -> Vec<String> {
    let tokens = sql_tokens(sql);
    let mut refs = Vec::new();
    let mut ctes = Vec::new();
    collect_relations(&tokens, &mut refs, &mut ctes);
    refs
}

fn collect_relations(tokens: &[SqlToken], refs: &mut Vec<String>, ctes: &mut Vec<String>) {
    let mut index = 0;
    let scope = ctes.len();
    while index < tokens.len() {
        if is_word(&tokens[index], "WITH") {
            index = consume_with(tokens, index, refs, ctes);
            continue;
        }
        if is_word(&tokens[index], "INSERT") {
            index = consume_insert(tokens, index, refs, ctes);
            continue;
        }
        if is_word(&tokens[index], "UPDATE") && is_table_update(tokens, index) {
            index = consume_update(tokens, index, refs, ctes);
            continue;
        }
        if is_word(&tokens[index], "FROM") || is_word(&tokens[index], "JOIN") {
            let next = parse_table_list(tokens, index + 1, refs, ctes);
            index = if next > index { next } else { index + 1 };
            continue;
        }
        // WHERE EXISTS 和标量子查询里的 FROM 也是引用，不能只扫本层。
        if tokens[index].text == "(" {
            let end = matching_paren(tokens, index);
            let mut nested = ctes.clone();
            collect_relations(&tokens[index + 1..end], refs, &mut nested);
            index = end.saturating_add(1);
            continue;
        }
        index += 1;
    }
    ctes.truncate(scope);
}

/// FOR UPDATE、ON UPDATE、DO UPDATE 不是改表。BEFORE/AFTER UPDATE 是触发器时机。
fn is_table_update(tokens: &[SqlToken], index: usize) -> bool {
    if index == 0 {
        return true;
    }
    let previous = &tokens[index - 1];
    !is_word(previous, "FOR") && !is_word(previous, "ON") && !is_word(previous, "DO") && !is_word(previous, "BEFORE") && !is_word(previous, "AFTER")
}

fn consume_insert(tokens: &[SqlToken], insert_at: usize, refs: &mut Vec<String>, ctes: &[String]) -> usize {
    let mut index = insert_at + 1;
    if index < tokens.len() && is_word(&tokens[index], "INTO") {
        index += 1;
    }
    if !starts_dml_target(tokens, index) {
        return insert_at + 1;
    }
    parse_dml_target(tokens, index, refs, ctes)
}

fn consume_update(tokens: &[SqlToken], update_at: usize, refs: &mut Vec<String>, ctes: &[String]) -> usize {
    let mut index = update_at + 1;
    if index < tokens.len() && is_word(&tokens[index], "ONLY") {
        index += 1;
    }
    if !starts_dml_target(tokens, index) {
        return update_at + 1;
    }
    parse_dml_target(tokens, index, refs, ctes)
}

fn starts_dml_target(tokens: &[SqlToken], index: usize) -> bool {
    if index >= tokens.len() {
        return false;
    }
    if tokens[index].text == "(" {
        return true;
    }
    is_ident_token(&tokens[index]) && !is_from_stop(&tokens[index].text) && !is_word(&tokens[index], "SET") && !is_word(&tokens[index], "VALUES") && !is_word(&tokens[index], "SELECT") && !is_word(&tokens[index], "DEFAULT")
}

fn parse_dml_target(tokens: &[SqlToken], mut index: usize, refs: &mut Vec<String>, ctes: &[String]) -> usize {
    if index < tokens.len() && (is_word(&tokens[index], "ONLY") || is_word(&tokens[index], "LATERAL")) {
        index += 1;
    }
    if index >= tokens.len() {
        return index;
    }
    if tokens[index].text == "(" {
        let end = matching_paren(tokens, index);
        let mut nested = ctes.to_vec();
        collect_relations(&tokens[index + 1..end], refs, &mut nested);
        return skip_alias(tokens, end.saturating_add(1));
    }
    if !starts_dml_target(tokens, index) {
        return index;
    }
    let (name, next) = qualified_relation(tokens, index);
    if !name.is_empty() && !ctes.iter().any(|cte| cte.eq_ignore_ascii_case(&name)) {
        refs.push(name);
    }
    index = next;
    if index < tokens.len() && tokens[index].text == "(" {
        index = matching_paren(tokens, index).saturating_add(1);
    }
    skip_alias(tokens, index)
}

/// 读完 WITH 绑定的 CTE，并让这些名字在后续主查询里可见。返回主查询起点。
fn consume_with(tokens: &[SqlToken], with_at: usize, refs: &mut Vec<String>, ctes: &mut Vec<String>) -> usize {
    let mut index = with_at + 1;
    if index < tokens.len() && is_word(&tokens[index], "RECURSIVE") {
        index += 1;
    }
    loop {
        if index >= tokens.len() || !is_ident_token(&tokens[index]) || is_from_stop(&tokens[index].text) {
            break;
        }
        let name = tokens[index].text.clone();
        let mut next = index + 1;
        if next < tokens.len() && tokens[next].text == "(" {
            let after = matching_paren(tokens, next).saturating_add(1);
            if after < tokens.len() && is_word(&tokens[after], "AS") {
                next = after;
            }
        }
        if next >= tokens.len() || !is_word(&tokens[next], "AS") {
            break;
        }
        next += 1;
        if next < tokens.len() && is_word(&tokens[next], "NOT") {
            next += 1;
        }
        if next < tokens.len() && is_word(&tokens[next], "MATERIALIZED") {
            next += 1;
        }
        if next >= tokens.len() || tokens[next].text != "(" {
            break;
        }
        let end = matching_paren(tokens, next);
        ctes.push(name);
        collect_relations(&tokens[next + 1..end], refs, ctes);
        index = end.saturating_add(1);
        if index < tokens.len() && tokens[index].text == "," {
            index += 1;
            continue;
        }
        break;
    }
    index
}

fn parse_table_list(tokens: &[SqlToken], mut index: usize, refs: &mut Vec<String>, ctes: &[String]) -> usize {
    while index < tokens.len() {
        if is_word(&tokens[index], "ONLY") || is_word(&tokens[index], "LATERAL") {
            index += 1;
            continue;
        }
        if tokens[index].text == "(" {
            let end = matching_paren(tokens, index);
            let mut nested = ctes.to_vec();
            collect_relations(&tokens[index + 1..end], refs, &mut nested);
            index = end.saturating_add(1);
            index = skip_alias(tokens, index);
        } else if is_ident_token(&tokens[index]) {
            let (name, next) = qualified_relation(tokens, index);
            if next < tokens.len() && tokens[next].text == "(" {
                index = matching_paren(tokens, next).saturating_add(1);
            } else {
                if !name.is_empty() && !ctes.iter().any(|cte| cte.eq_ignore_ascii_case(&name)) {
                    refs.push(name);
                }
                index = next;
            }
            index = skip_alias(tokens, index);
        } else {
            break;
        }
        if index < tokens.len() && tokens[index].text == "," {
            index += 1;
            continue;
        }
        break;
    }
    index
}

fn qualified_relation(tokens: &[SqlToken], start: usize) -> (String, usize) {
    let parts = qualified_parts(&tokens[start..]);
    if parts.is_empty() {
        return (String::new(), start);
    }
    let mut index = start + 1;
    let mut seen = 1;
    while seen < parts.len() {
        if index + 1 < tokens.len() && tokens[index].text == "." {
            index += 2;
            seen += 1;
        } else {
            break;
        }
    }
    (parts.last().cloned().unwrap_or_default(), index)
}

fn matching_paren(tokens: &[SqlToken], open: usize) -> usize {
    let mut depth = 0;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        if token.text == "(" {
            depth += 1;
        } else if token.text == ")" {
            depth -= 1;
            if depth == 0 {
                return index;
            }
        }
    }
    tokens.len().saturating_sub(1)
}

fn skip_alias(tokens: &[SqlToken], mut index: usize) -> usize {
    if index < tokens.len() && is_word(&tokens[index], "AS") {
        index += 1;
    }
    if index < tokens.len() && is_ident_token(&tokens[index]) && !is_from_stop(&tokens[index].text) {
        index += 1;
    }
    index
}

fn is_from_stop(word: &str) -> bool {
    matches!(
        word.to_ascii_uppercase().as_str(),
        "WHERE" | "GROUP" | "HAVING" | "WINDOW" | "ORDER" | "LIMIT" | "OFFSET" | "UNION" | "EXCEPT" | "INTERSECT" | "FETCH" | "FOR" | "ON" | "USING" | "LEFT" | "RIGHT" | "INNER" | "OUTER" | "FULL" | "CROSS" | "NATURAL" | "JOIN" | "SELECT" | "SET" | "WHEN" | "THEN" | "ELSE" | "END" | "AND" | "OR"
    )
}

struct SqlToken {
    text: String,
    quoted: bool,
}

fn sql_tokens(sql: &str) -> Vec<SqlToken> {
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < sql.len() {
        let ch = sql[index..].chars().next().unwrap();
        // 函数体在美元引号里。整段跳过会看不见其中的调用，创建顺序就退回名称序。
        if ch == '$' {
            if let Some(end) = skip_dollar(sql, index) {
                let delimiter_len = dollar_delimiter_len(sql, index).unwrap_or(0);
                let body_start = index + delimiter_len;
                let body_end = end.saturating_sub(delimiter_len);
                if body_end > body_start {
                    tokens.extend(sql_tokens(&sql[body_start..body_end]));
                }
                index = end;
                continue;
            }
        }
        if let Some(next) = skip_delimited(sql, index) {
            let ch = sql[index..].chars().next().unwrap();
            if ch == '"' || ch == '`' || ch == '[' {
                tokens.push(SqlToken { text: unquote(&sql[index..next]), quoted: true });
            }
            index = next;
            continue;
        }
        if ch.is_whitespace() {
            index += ch.len_utf8();
            continue;
        }
        if is_ident_start(ch) {
            let end = ident_continue(sql, index);
            tokens.push(SqlToken { text: sql[index..end].to_owned(), quoted: false });
            index = end;
            continue;
        }
        tokens.push(SqlToken { text: ch.to_string(), quoted: false });
        index += ch.len_utf8();
    }
    tokens
}

fn unquote(raw: &str) -> String {
    let closer = if raw.starts_with('[') { ']' } else { raw.chars().next().unwrap_or('"') };
    let open = if closer == ']' { '[' } else { closer };
    let inner = raw.trim_start_matches(open).trim_end_matches(closer);
    inner.replace(&format!("{closer}{closer}"), &closer.to_string())
}

fn is_word(token: &SqlToken, word: &str) -> bool {
    !token.quoted && token.text.eq_ignore_ascii_case(word)
}

fn is_ident_token(token: &SqlToken) -> bool {
    token.quoted || token.text.chars().next().is_some_and(is_ident_start)
}

fn statement_mentions(sql: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    sql_tokens(sql).into_iter().any(|token| if token.quoted { token.text == name } else { token.text.eq_ignore_ascii_case(name) })
}

/// 入度是被同批对象引用的次数。先删没有入边的对象，也就是先删依赖方。成环的留在后面；表之间的环靠已经发出的 DROP CONSTRAINT 解开。
fn order_dependents_first(tables: Vec<PlannedDrop>) -> Vec<PlannedDrop> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut by_name: BTreeMap<String, Vec<PlannedDrop>> = BTreeMap::new();
    for table in tables {
        by_name.entry(table.name.clone()).or_default().push(table);
    }
    let mut outgoing: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut indegree: BTreeMap<String, usize> = BTreeMap::new();
    for name in by_name.keys() {
        outgoing.insert(name.clone(), BTreeSet::new());
        indegree.insert(name.clone(), 0);
    }
    for group in by_name.values() {
        for table in group {
            for referenced in &table.references {
                if referenced == &table.name || !by_name.contains_key(referenced) {
                    continue;
                }
                if outgoing.get_mut(&table.name).expect("table").insert(referenced.clone()) {
                    *indegree.get_mut(referenced).expect("referenced") += 1;
                }
            }
        }
    }
    let mut ready: BTreeSet<String> = indegree.iter().filter(|(_, degree)| **degree == 0).map(|(name, _)| name.clone()).collect();
    let mut ordered = Vec::new();
    while let Some(name) = ready.iter().next().cloned() {
        ready.remove(&name);
        ordered.push(name.clone());
        for parent in outgoing.get(&name).into_iter().flatten() {
            let degree = indegree.get_mut(parent).expect("parent");
            *degree -= 1;
            if *degree == 0 {
                ready.insert(parent.clone());
            }
        }
    }
    let mut cyclic = indegree
        .iter()
        .filter(|(name, degree)| **degree > 0 && !ordered.iter().any(|item| item == *name))
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    cyclic.sort();
    ordered.extend(cyclic);
    let mut result = Vec::new();
    for name in ordered {
        if let Some(group) = by_name.remove(&name) {
            result.extend(group);
        }
    }
    result
}

/// 先输出被引用的对象。视图创建用这个顺序；成环的留在最后，不按名称把被引用方提前删掉。
fn order_dependencies_first(items: Vec<PlannedDrop>) -> Vec<PlannedDrop> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut by_name: BTreeMap<String, Vec<PlannedDrop>> = BTreeMap::new();
    for item in items {
        by_name.entry(item.name.clone()).or_default().push(item);
    }
    let mut dependents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut indegree: BTreeMap<String, usize> = BTreeMap::new();
    for name in by_name.keys() {
        dependents.insert(name.clone(), BTreeSet::new());
        indegree.insert(name.clone(), 0);
    }
    for group in by_name.values() {
        for item in group {
            for referenced in &item.references {
                if referenced.eq_ignore_ascii_case(&item.name) || !by_name.contains_key(referenced) {
                    continue;
                }
                if dependents.get_mut(referenced).expect("referenced").insert(item.name.clone()) {
                    *indegree.get_mut(&item.name).expect("item") += 1;
                }
            }
        }
    }
    let mut ready: BTreeSet<String> = indegree.iter().filter(|(_, degree)| **degree == 0).map(|(name, _)| name.clone()).collect();
    let mut ordered = Vec::new();
    while let Some(name) = ready.iter().next().cloned() {
        ready.remove(&name);
        ordered.push(name.clone());
        for dependent in dependents.get(&name).into_iter().flatten() {
            let degree = indegree.get_mut(dependent).expect("dependent");
            *degree -= 1;
            if *degree == 0 {
                ready.insert(dependent.clone());
            }
        }
    }
    let mut cyclic = indegree
        .iter()
        .filter(|(name, degree)| **degree > 0 && !ordered.iter().any(|item| item == *name))
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    cyclic.sort();
    ordered.extend(cyclic);
    let mut result = Vec::new();
    for name in ordered {
        if let Some(group) = by_name.remove(&name) {
            result.extend(group);
        }
    }
    result
}

fn replaces_in_place(engine: Engine, base: &ObjectDef, other: &ObjectDef) -> bool {
    match base.kind {
        Kind::View => match engine {
            Engine::Mysql | Engine::Mariadb => !base.materialized && !other.materialized,
            Engine::Postgres => !base.materialized && !other.materialized && view_columns_compatible(&base.definition, &other.definition),
            Engine::Sqlite => false,
        },
        // 返回类型变了时 CREATE OR REPLACE FUNCTION 会被拒绝，只能先摘依赖再删。
        Kind::Function | Kind::Procedure => matches!(engine, Engine::Postgres | Engine::Mariadb) && return_types_compatible(&base.definition, &other.definition),
        _ => false,
    }
}

fn view_columns_compatible(wanted: &str, current: &str) -> bool {
    // 缺列清单时看不出旧输出列是否还在。pg_get_viewdef 通常没有列清单，不能当成可以 OR REPLACE。
    match (explicit_view_columns(wanted), explicit_view_columns(current)) {
        (Some(wanted), Some(current)) => wanted.len() >= current.len() && wanted[..current.len()] == current[..],
        _ => false,
    }
}

/// 替换后的定义不再依赖即将删除的列、函数、表或视图时，把 CREATE OR REPLACE 放到这些语句之前。
/// 旧正文若还指向即将删除的视图，删视图发生在替换之前，提前替换会被随后的 DROP 吃掉且不再建回。
/// 新正文若改去引用本次会 DROP 的视图，表、例程、列三条都不能提前，要降成先删后建。
fn is_early_view_replace(
    create: &PlannedCreate,
    alters: &[String],
    detach: &[String],
    routine_drops: &[String],
    dropped_tables: &[String],
    dropped_views: &[String],
) -> bool {
    if create.current_definition.is_empty() {
        return false;
    }
    let current = &create.current_definition;
    let wanted = &create.definition;
    let references = |definition: &str, names: &[String]| {
        referenced_relations(definition).iter().any(|found| names.iter().any(|name| found.eq_ignore_ascii_case(name)))
    };
    if references(current, dropped_views) {
        return false;
    }
    let wanted_still_depends = references(wanted, dropped_tables)
        || references(wanted, dropped_views)
        || routine_drops.iter().any(|name| calls_routine(wanted, name));
    if wanted_still_depends {
        return false;
    }
    if references(current, dropped_tables) {
        return true;
    }
    if routine_drops.iter().any(|name| statement_mentions(current, name)) {
        return true;
    }
    blocking_columns(alters).into_iter().chain(blocking_columns(detach)).any(|(table, column)| view_uses_table_column(current, &table, &column))
}

fn explicit_view_columns(definition: &str) -> Option<Vec<String>> {
    let tokens = sql_tokens(definition);
    let view_at = tokens.iter().position(|token| is_word(token, "VIEW"))?;
    let mut index = view_at + 1;
    if index >= tokens.len() || !is_ident_token(&tokens[index]) {
        return None;
    }
    index += 1;
    while index + 1 < tokens.len() && tokens[index].text == "." && is_ident_token(&tokens[index + 1]) {
        index += 2;
    }
    if index >= tokens.len() || tokens[index].text != "(" {
        return None;
    }
    let end = matching_paren(&tokens, index);
    let names = tokens[index + 1..end].iter().filter(|token| is_ident_token(token)).map(|token| token.text.clone()).collect::<Vec<_>>();
    if names.is_empty() {
        None
    } else {
        Some(names)
    }
}

fn return_types_compatible(left: &str, right: &str) -> bool {
    match (returns_clause(left), returns_clause(right)) {
        (None, None) => true,
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn returns_clause(definition: &str) -> Option<String> {
    let tokens = sql_tokens(definition);
    let index = tokens.iter().position(|token| is_word(token, "RETURNS"))?;
    let mut parts = Vec::new();
    for token in tokens.iter().skip(index + 1) {
        if matches!(
            token.text.to_ascii_uppercase().as_str(),
            "LANGUAGE" | "AS" | "WINDOW" | "IMMUTABLE" | "STABLE" | "VOLATILE" | "LEAKPROOF" | "CALLED" | "STRICT" | "COST" | "ROWS" | "PARALLEL" | "SECURITY" | "SET" | "TRANSFORM" | "NOT"
        ) && !token.quoted
        {
            break;
        }
        parts.push(token.text.to_ascii_lowercase());
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn ensure_or_replace_kind(sql: &str, kind: Kind) -> String {
    match kind {
        Kind::View | Kind::Function | Kind::Procedure => ensure_or_replace(sql),
        _ => sql.to_owned(),
    }
}

fn ensure_or_replace(sql: &str) -> String {
    let trimmed = sql.trim_start();
    let offset = sql.len() - trimmed.len();
    if trimmed.len() < 6 || !trimmed[..6].eq_ignore_ascii_case("CREATE") {
        return sql.to_owned();
    }
    let rest = trimmed[6..].trim_start();
    if rest.len() >= 10 && rest[..10].eq_ignore_ascii_case("OR REPLACE") {
        return sql.to_owned();
    }
    format!("{}CREATE OR REPLACE{}", &sql[..offset], &sql[offset + 6..])
}

/// 仍被列引用的枚举、域和序列不先 DROP。视图、函数和触发器如果还指向即将删除的对象，先摘掉再按原定义建回。
fn preserve_dependents(
    engine: Engine,
    baseline: &Side,
    target: &Side,
    drops: &mut Vec<PlannedDrop>,
    creates: &mut Vec<PlannedCreate>,
    alters: &[String],
    in_place: &mut Vec<String>,
    detach: &mut Vec<String>,
    restore: &mut Vec<String>,
) {
    rewrite_kept_types(engine, target, baseline, drops, creates, in_place, detach, restore);
    rewrite_kept_sequences(engine, target, baseline, drops, creates, alters, in_place, detach, restore);
    detach_routine_defaults(engine, baseline, target, drops, alters, detach, restore);
    cover_dependents(engine, baseline, target, drops, creates, alters, detach);
    demote_blocked_view_replacements(engine, baseline, target, drops, creates, alters, detach);
    cover_dependents(engine, baseline, target, drops, creates, alters, detach);
}

fn rewrite_kept_types(
    engine: Engine,
    target: &Side,
    baseline: &Side,
    drops: &mut Vec<PlannedDrop>,
    creates: &mut Vec<PlannedCreate>,
    in_place: &mut Vec<String>,
    detach: &mut Vec<String>,
    restore: &mut Vec<String>,
) {
    if engine != Engine::Postgres {
        return;
    }
    let paired = drops
        .iter()
        .filter(|drop| matches!(drop.kind, Kind::Enum | Kind::Domain) && creates.iter().any(|create| create.key == drop.key))
        .map(|drop| (drop.kind, drop.name.clone(), drop.key.clone()))
        .collect::<Vec<_>>();
    for (kind, name, key) in paired {
        let target_cols = columns_of_type(target, &name);
        if target_cols.is_empty() {
            continue;
        }
        if kind == Kind::Enum {
            if let (Some(wanted), Some(current)) = (object_definition(baseline, Kind::Enum, &name), object_definition(target, Kind::Enum, &name)) {
                if let (Some(wanted_labels), Some(current_labels)) = (enum_labels(wanted), enum_labels(current)) {
                    if let Some(adds) = enum_additions(&current_labels, &wanted_labels) {
                        remove_planned(&key, drops, creates);
                        for (label, before) in adds {
                            in_place.push(enum_add_sql(engine, target, &name, &label, before.as_deref()));
                        }
                        continue;
                    }
                }
            }
        }
        let baseline_cols = columns_of_type(baseline, &name);
        for (table, column) in &target_cols {
            detach.push(format!(
                "ALTER TABLE {} ALTER COLUMN {} TYPE text USING {}::text",
                qualify(engine, target, table),
                quote(engine, column),
                quote(engine, column)
            ));
            if baseline_cols.iter().any(|(base_table, base_column)| base_table == table && base_column == column) {
                let type_name = qualify(engine, target, &name);
                restore.push(format!(
                    "ALTER TABLE {} ALTER COLUMN {} TYPE {type_name} USING {}::{type_name}",
                    qualify(engine, target, table),
                    quote(engine, column),
                    quote(engine, column)
                ));
            }
        }
    }
}

fn rewrite_kept_sequences(
    engine: Engine,
    target: &Side,
    baseline: &Side,
    drops: &mut Vec<PlannedDrop>,
    creates: &mut Vec<PlannedCreate>,
    alters: &[String],
    in_place: &mut Vec<String>,
    detach: &mut Vec<String>,
    restore: &mut Vec<String>,
) {
    if engine != Engine::Postgres {
        return;
    }
    let paired = drops
        .iter()
        .filter(|drop| drop.kind == Kind::Sequence && creates.iter().any(|create| create.key == drop.key))
        .map(|drop| (drop.name.clone(), drop.key.clone()))
        .collect::<Vec<_>>();
    for (name, key) in paired {
        let wanted = object_definition(baseline, Kind::Sequence, &name);
        let current = object_definition(target, Kind::Sequence, &name);
        if let (Some(wanted), Some(current)) = (wanted, current) {
            if let (Some(wanted_spec), Some(current_spec)) = (parse_sequence(wanted), parse_sequence(current)) {
                let same_type = wanted_spec.data_type.is_empty() || current_spec.data_type.is_empty() || wanted_spec.data_type.eq_ignore_ascii_case(&current_spec.data_type);
                if same_type {
                    remove_planned(&key, drops, creates);
                    in_place.push(sequence_alter_sql(engine, target, &name, &wanted_spec));
                    continue;
                }
            }
        }
        detach_sequence_use(engine, target, baseline, &name, alters, detach, restore);
    }
}

fn detach_sequence_use(engine: Engine, target: &Side, baseline: &Side, name: &str, alters: &[String], detach: &mut Vec<String>, restore: &mut Vec<String>) {
    for object in target.objects.iter().filter(|object| object.kind == Kind::Table) {
        let Some(shape) = object.table.as_ref() else { continue };
        for column in &shape.columns {
            if !default_references_sequence(&column.default_sql, name) {
                continue;
            }
            if !changes_default(alters, &object.name, &column.name, true) && !changes_default(detach, &object.name, &column.name, true) {
                detach.push(drop_default_sql(engine, target, &object.name, &column.name));
            }
            if let Some(base_column) = baseline_column(baseline, &object.name, &column.name) {
                if !base_column.default_sql.trim().is_empty() && !changes_default(alters, &object.name, &column.name, false) && !changes_default(restore, &object.name, &column.name, false) {
                    restore.push(set_default_sql(engine, target, &object.name, &column.name, base_column.default_sql.trim()));
                }
            }
        }
    }
    if let Some(sequence) = baseline.objects.iter().find(|object| object.kind == Kind::Sequence && object.name.eq_ignore_ascii_case(name)) {
        let (owner_table, owner_column) = object_sequence_owner(sequence);
        if let (Some(table), Some(column)) = (owner_table, owner_column) {
            restore.push(format!("ALTER SEQUENCE {} OWNED BY {}.{}", qualify(engine, target, name), qualify(engine, target, &table), quote(engine, &column)));
        }
    }
}

fn detach_routine_defaults(
    engine: Engine,
    baseline: &Side,
    target: &Side,
    drops: &[PlannedDrop],
    alters: &[String],
    detach: &mut Vec<String>,
    restore: &mut Vec<String>,
) {
    let routines = drops.iter().filter(|drop| matches!(drop.kind, Kind::Function | Kind::Procedure)).map(|drop| drop.name.clone()).collect::<Vec<_>>();
    for name in routines {
        for object in target.objects.iter().filter(|object| object.kind == Kind::Table) {
            let Some(shape) = object.table.as_ref() else { continue };
            for column in &shape.columns {
                if !statement_mentions(&column.default_sql, &name) {
                    continue;
                }
                if !changes_default(alters, &object.name, &column.name, true) && !changes_default(detach, &object.name, &column.name, true) {
                    detach.push(drop_default_sql(engine, target, &object.name, &column.name));
                }
                if let Some(base_column) = baseline_column(baseline, &object.name, &column.name) {
                    if !base_column.default_sql.trim().is_empty()
                        && !changes_default(alters, &object.name, &column.name, false)
                        && !changes_default(restore, &object.name, &column.name, false)
                    {
                        restore.push(set_default_sql(engine, target, &object.name, &column.name, base_column.default_sql.trim()));
                    }
                }
            }
        }
    }
}

fn cover_dependents(
    engine: Engine,
    baseline: &Side,
    target: &Side,
    drops: &mut Vec<PlannedDrop>,
    creates: &mut Vec<PlannedCreate>,
    alters: &[String],
    detach: &[String],
) {
    let objects = target.objects.clone();
    let blocked = blocking_columns(alters).into_iter().chain(blocking_columns(detach)).collect::<Vec<_>>();
    for _ in 0..objects.len().saturating_add(1) {
        let dropped_views = drops.iter().filter(|drop| drop.kind == Kind::View).map(|drop| drop.name.clone()).collect::<Vec<_>>();
        let dropped_relations = drops
            .iter()
            .filter(|drop| matches!(drop.kind, Kind::View | Kind::Table))
            .map(|drop| drop.name.clone())
            .collect::<Vec<_>>();
        let dropped_routines = drops
            .iter()
            .filter(|drop| matches!(drop.kind, Kind::Function | Kind::Procedure))
            .map(|drop| drop.name.clone())
            .collect::<Vec<_>>();
        let mut progressed = false;
        for object in &objects {
            if planned_drop(drops, object) {
                continue;
            }
            let hit = match object.kind {
                // 正在原地替换的视图看新定义：新定义已经不再依赖时，不必先 DROP。
                // 其余视图除了 FROM/JOIN，选择被改写的列或调用即将删除的函数也会挡住后面的语句。
                Kind::View => {
                    let current_body = object.definition.as_str();
                    // 列和函数可以靠更早的 OR REPLACE 解开；视图依赖不行，因为删视图发生在替换之前。
                    let next_body = creates
                        .iter()
                        .find(|create| create.key == identity(object) && !create.current_definition.is_empty())
                        .map(|create| create.definition.as_str())
                        .unwrap_or(current_body);
                    referenced_relations(current_body).iter().any(|name| dropped_views.iter().any(|drop| drop.eq_ignore_ascii_case(name)))
                        || dropped_routines.iter().any(|name| calls_routine(next_body, name))
                        || blocked.iter().any(|(table, column)| view_uses_table_column(next_body, table, column))
                }
                Kind::Trigger => {
                    dropped_relations.iter().any(|name| name.eq_ignore_ascii_case(&object.relation))
                        || dropped_routines.iter().any(|name| calls_routine(&object.definition, name))
                }
                // 例程正文里的 FROM/JOIN、INSERT、UPDATE 会挡住 DROP VIEW。调用才算依赖其它例程。
                Kind::Function | Kind::Procedure => {
                    let mentions_routine = dropped_routines.iter().any(|name| !name.eq_ignore_ascii_case(&object.name) && calls_routine(&object.definition, name));
                    let mentions_view = referenced_relations(&object.definition)
                        .iter()
                        .any(|found| dropped_views.iter().any(|name| found.eq_ignore_ascii_case(name)));
                    mentions_routine || mentions_view
                }
                _ => false,
            };
            if !hit {
                continue;
            }
            drops.push(plan_drop(engine, target, object));
            if !planned_create(creates, object) {
                let source = same_object(baseline, object).unwrap_or(object);
                if let Some(sql) = recreate_sql(engine, target, source) {
                    creates.push(plan_create(source, sql));
                }
            }
            progressed = true;
        }
        if !progressed {
            break;
        }
    }
}

/// 新定义仍然依赖即将删除的函数或列，或者要用到还没建出来、以及本次会先 DROP 的视图时，不能先 OR REPLACE。
fn demote_blocked_view_replacements(
    engine: Engine,
    baseline: &Side,
    target: &Side,
    drops: &mut Vec<PlannedDrop>,
    creates: &mut Vec<PlannedCreate>,
    alters: &[String],
    detach: &[String],
) {
    let routine_drops = drops
        .iter()
        .filter(|drop| matches!(drop.kind, Kind::Function | Kind::Procedure))
        .map(|drop| drop.name.clone())
        .collect::<Vec<_>>();
    // 即将删除的视图即使还留着旧定义，重建前也不存在。不能只把 current_definition 为空的对象当成 fresh。
    let mut fresh_names = creates.iter().filter(|create| create.current_definition.is_empty()).map(|create| create.name.clone()).collect::<Vec<_>>();
    let mut dropped_views = Vec::new();
    for drop in drops.iter().filter(|drop| drop.kind == Kind::View) {
        if !dropped_views.iter().any(|name: &String| name.eq_ignore_ascii_case(&drop.name)) {
            dropped_views.push(drop.name.clone());
        }
        if !fresh_names.iter().any(|name| name.eq_ignore_ascii_case(&drop.name)) {
            fresh_names.push(drop.name.clone());
        }
    }
    let added = added_columns(alters);
    let blocked = blocking_columns(alters).into_iter().chain(blocking_columns(detach)).collect::<Vec<_>>();
    let keys = creates
        .iter()
        .filter(|create| create.kind == Kind::View && !create.current_definition.is_empty())
        .filter(|create| {
            let wanted = &create.definition;
            let current = &create.current_definition;
            let keeps_dropped_routine = routine_drops.iter().any(|name| calls_routine(wanted, name));
            let keeps_blocked_column = blocked.iter().any(|(table, column)| view_uses_table_column(wanted, table, column));
            let needs_fresh = fresh_names
                .iter()
                .any(|name| !name.eq_ignore_ascii_case(&create.name) && statement_mentions(wanted, name) && !statement_mentions(current, name));
            let needs_dropped_view = referenced_relations(wanted).iter().any(|found| {
                dropped_views.iter().any(|name: &String| !name.eq_ignore_ascii_case(&create.name) && found.eq_ignore_ascii_case(name))
            });
            let needs_added = added.iter().any(|(table, column)| view_uses_table_column(wanted, table, column) && !view_uses_table_column(current, table, column));
            keeps_dropped_routine || keeps_blocked_column || needs_fresh || needs_dropped_view || needs_added
        })
        .map(|create| create.key.clone())
        .collect::<Vec<_>>();
    for key in keys {
        let Some(index) = creates.iter().position(|create| create.key == key) else {
            continue;
        };
        creates.remove(index);
        let Some(current) = target.objects.iter().find(|object| identity(object) == key) else {
            continue;
        };
        if !drops.iter().any(|drop| drop.key == key) {
            drops.push(plan_drop(engine, target, current));
        }
        let source = baseline.objects.iter().find(|object| identity(object) == key).unwrap_or(current);
        if let Some(sql) = create_definition(engine, target, source) {
            creates.push(plan_create(source, sql));
        }
    }
}

fn blocking_columns(statements: &[String]) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for sql in statements {
        if let Some(pair) = blocked_column(sql) {
            if !found.iter().any(|item| item == &pair) {
                found.push(pair);
            }
        }
    }
    found
}

fn added_columns(statements: &[String]) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for sql in statements {
        if let Some(pair) = added_column(sql) {
            if !found.iter().any(|item| item == &pair) {
                found.push(pair);
            }
        }
    }
    found
}

fn blocked_column(sql: &str) -> Option<(String, String)> {
    let tokens = sql_tokens(sql);
    let table_at = tokens.iter().position(|token| is_word(token, "TABLE"))?;
    let (table, mut index) = qualified_relation(&tokens, table_at + 1);
    if table.is_empty() {
        return None;
    }
    while index < tokens.len() {
        if is_word(&tokens[index], "DROP") && tokens.get(index + 1).is_some_and(|token| is_word(token, "COLUMN")) {
            let column = tokens.get(index + 2)?.text.clone();
            return Some((table, column));
        }
        if is_word(&tokens[index], "ALTER") && tokens.get(index + 1).is_some_and(|token| is_word(token, "COLUMN")) {
            let column = tokens.get(index + 2)?.text.clone();
            if tokens[index + 3..].iter().any(|token| is_word(token, "TYPE")) {
                return Some((table, column));
            }
        }
        index += 1;
    }
    None
}

fn added_column(sql: &str) -> Option<(String, String)> {
    let tokens = sql_tokens(sql);
    let table_at = tokens.iter().position(|token| is_word(token, "TABLE"))?;
    let (table, mut index) = qualified_relation(&tokens, table_at + 1);
    if table.is_empty() {
        return None;
    }
    while index + 1 < tokens.len() {
        if is_word(&tokens[index], "ADD") && is_word(&tokens[index + 1], "COLUMN") {
            let column = tokens.get(index + 2)?.text.clone();
            return Some((table, column));
        }
        index += 1;
    }
    None
}

fn view_uses_table_column(definition: &str, table: &str, column: &str) -> bool {
    if table.is_empty() || column.is_empty() {
        return false;
    }
    let uses_table = referenced_relations(definition).iter().any(|name| name.eq_ignore_ascii_case(table));
    if !uses_table {
        return false;
    }
    select_list_has_star(definition) || statement_mentions(definition, column)
}

fn select_list_has_star(definition: &str) -> bool {
    let tokens = sql_tokens(definition);
    let mut index = 0;
    while index < tokens.len() {
        if is_word(&tokens[index], "SELECT") {
            index += 1;
            while index < tokens.len() && !is_word(&tokens[index], "FROM") {
                if tokens[index].text == "*" {
                    return true;
                }
                index += 1;
            }
        }
        index += 1;
    }
    false
}

fn recreate_sql(engine: Engine, side: &Side, source: &ObjectDef) -> Option<String> {
    let sql = create_definition(engine, side, source)?;
    if source.materialized || !matches!(source.kind, Kind::View | Kind::Function | Kind::Procedure) {
        Some(sql)
    } else {
        Some(ensure_or_replace(&sql))
    }
}

fn remove_planned(key: &str, drops: &mut Vec<PlannedDrop>, creates: &mut Vec<PlannedCreate>) {
    drops.retain(|drop| drop.key != key);
    creates.retain(|create| create.key != key);
}

fn planned_drop(drops: &[PlannedDrop], object: &ObjectDef) -> bool {
    let key = identity(object);
    drops.iter().any(|drop| drop.key == key)
}

fn planned_create(creates: &[PlannedCreate], object: &ObjectDef) -> bool {
    let key = identity(object);
    creates.iter().any(|create| create.key == key)
}

fn same_object<'a>(side: &'a Side, object: &ObjectDef) -> Option<&'a ObjectDef> {
    let key = identity(object);
    side.objects.iter().find(|item| identity(item) == key)
}

fn object_definition<'a>(side: &'a Side, kind: Kind, name: &str) -> Option<&'a str> {
    side.objects.iter().find(|object| object.kind == kind && object.name.eq_ignore_ascii_case(name)).map(|object| object.definition.as_str())
}

fn columns_of_type<'a>(side: &'a Side, type_name: &str) -> Vec<(&'a str, &'a str)> {
    let mut found = Vec::new();
    for object in side.objects.iter().filter(|object| object.kind == Kind::Table) {
        let Some(shape) = object.table.as_ref() else { continue };
        for column in &shape.columns {
            if type_mentions(&column.type_name, type_name) {
                found.push((object.name.as_str(), column.name.as_str()));
            }
        }
    }
    found
}

fn type_mentions(type_name: &str, object_name: &str) -> bool {
    if object_name.is_empty() || type_name.trim().is_empty() {
        return false;
    }
    let mut bare = type_name.trim().trim_end_matches("[]").trim();
    if let Some(end) = bare.find('(') {
        bare = bare[..end].trim();
    }
    let last = bare.rsplit(|ch: char| ch == '.' || ch == '"' || ch == '`').find(|part| !part.is_empty()).unwrap_or(bare);
    last.eq_ignore_ascii_case(object_name)
}

fn baseline_column<'a>(baseline: &'a Side, table: &str, column: &str) -> Option<&'a ColumnShape> {
    let object = baseline.objects.iter().find(|object| object.kind == Kind::Table && object.name == table)?;
    object.table.as_ref()?.columns.iter().find(|item| item.name == column)
}

fn changes_default(statements: &[String], table: &str, column: &str, drop: bool) -> bool {
    let needle = if drop { "DROP DEFAULT" } else { "SET DEFAULT" };
    statements.iter().any(|sql| sql.to_ascii_uppercase().contains(needle) && statement_mentions(sql, table) && statement_mentions(sql, column))
}

fn drop_default_sql(engine: Engine, side: &Side, table: &str, column: &str) -> String {
    let table = qualify(engine, side, table);
    let column = quote(engine, column);
    match engine {
        Engine::Mysql | Engine::Mariadb => format!("ALTER TABLE {table} ALTER {column} DROP DEFAULT"),
        _ => format!("ALTER TABLE {table} ALTER COLUMN {column} DROP DEFAULT"),
    }
}

fn set_default_sql(engine: Engine, side: &Side, table: &str, column: &str, expression: &str) -> String {
    let table = qualify(engine, side, table);
    let column = quote(engine, column);
    match engine {
        Engine::Mysql | Engine::Mariadb => format!("ALTER TABLE {table} ALTER {column} SET DEFAULT {expression}"),
        _ => format!("ALTER TABLE {table} ALTER COLUMN {column} SET DEFAULT {expression}"),
    }
}

fn object_sequence_owner(object: &ObjectDef) -> (Option<String>, Option<String>) {
    let relation = object.relation.trim();
    if !relation.is_empty() {
        let mut parts = relation.split('\u{1}');
        return (parts.next().map(str::to_owned), parts.next().map(str::to_owned));
    }
    owned_by_clause(&object.definition)
}

fn enum_labels(definition: &str) -> Option<Vec<String>> {
    let end = find_keyword_end(definition, "ENUM")?;
    let start = skip_gap(definition, end);
    if !definition[start..].starts_with('(') {
        return None;
    }
    let close = raw_matching_paren(definition, start)?;
    let labels = string_literals(&definition[start + 1..close]);
    if labels.is_empty() {
        None
    } else {
        Some(labels)
    }
}

fn raw_matching_paren(sql: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut index = open;
    while index < sql.len() {
        if let Some(next) = skip_delimited(sql, index) {
            index = next;
            continue;
        }
        let ch = sql[index..].chars().next()?;
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
        index += ch.len_utf8();
    }
    None
}

/// 只能加标签时返回新增项，以及应插到哪个已有标签之前。删除或换序返回 None。
fn enum_additions(current: &[String], wanted: &[String]) -> Option<Vec<(String, Option<String>)>> {
    if current.iter().any(|label| !wanted.iter().any(|item| item == label)) {
        return None;
    }
    let mut cursor = 0;
    let mut adds = Vec::new();
    for (index, label) in wanted.iter().enumerate() {
        if cursor < current.len() && &current[cursor] == label {
            cursor += 1;
            continue;
        }
        if current.iter().any(|item| item == label) {
            return None;
        }
        let before = wanted[index + 1..].iter().find(|item| current.iter().any(|old| old == *item)).cloned();
        adds.push((label.clone(), before));
    }
    if cursor != current.len() {
        None
    } else {
        Some(adds)
    }
}

fn enum_add_sql(engine: Engine, side: &Side, name: &str, label: &str, before: Option<&str>) -> String {
    let mut sql = format!("ALTER TYPE {} ADD VALUE IF NOT EXISTS {}", qualify(engine, side, name), quote_literal(label));
    if let Some(before) = before {
        sql.push_str(" BEFORE ");
        sql.push_str(&quote_literal(before));
    }
    sql
}

fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

struct SequenceSpec {
    data_type: String,
    increment: String,
    min_value: String,
    max_value: String,
    start: String,
    cache: String,
    cycle: bool,
}

fn parse_sequence(definition: &str) -> Option<SequenceSpec> {
    let tokens = loose_tokens(definition);
    let seq_at = tokens.iter().position(|token| token.eq_ignore_ascii_case("SEQUENCE"))?;
    let mut index = seq_at + 1;
    while index < tokens.len() && !is_sequence_keyword(&tokens[index]) {
        index += 1;
    }
    let mut spec = SequenceSpec {
        data_type: String::new(),
        increment: String::new(),
        min_value: String::new(),
        max_value: String::new(),
        start: String::new(),
        cache: String::new(),
        cycle: false,
    };
    while index < tokens.len() {
        let word = tokens[index].as_str();
        if word.eq_ignore_ascii_case("AS") {
            spec.data_type = tokens.get(index + 1)?.clone();
            index += 2;
        } else if word.eq_ignore_ascii_case("INCREMENT") {
            index = skip_filler(&tokens, index);
            spec.increment = tokens.get(index)?.clone();
            index += 1;
        } else if word.eq_ignore_ascii_case("MINVALUE") {
            spec.min_value = tokens.get(index + 1)?.clone();
            index += 2;
        } else if word.eq_ignore_ascii_case("MAXVALUE") {
            spec.max_value = tokens.get(index + 1)?.clone();
            index += 2;
        } else if word.eq_ignore_ascii_case("START") {
            index = skip_filler(&tokens, index);
            spec.start = tokens.get(index)?.clone();
            index += 1;
        } else if word.eq_ignore_ascii_case("CACHE") {
            spec.cache = tokens.get(index + 1)?.clone();
            index += 2;
        } else if word.eq_ignore_ascii_case("CYCLE") {
            spec.cycle = true;
            index += 1;
        } else if word.eq_ignore_ascii_case("NO") {
            let next = tokens.get(index + 1).map(String::as_str).unwrap_or("");
            if next.eq_ignore_ascii_case("CYCLE") {
                spec.cycle = false;
                index += 2;
            } else if next.eq_ignore_ascii_case("MINVALUE") {
                spec.min_value.clear();
                index += 2;
            } else if next.eq_ignore_ascii_case("MAXVALUE") {
                spec.max_value.clear();
                index += 2;
            } else {
                index += 1;
            }
        } else {
            index += 1;
        }
    }
    if spec.increment.is_empty() {
        None
    } else {
        Some(spec)
    }
}

fn is_sequence_keyword(word: &str) -> bool {
    matches!(
        word.to_ascii_uppercase().as_str(),
        "AS" | "INCREMENT" | "MINVALUE" | "MAXVALUE" | "START" | "RESTART" | "CACHE" | "CYCLE" | "NO" | "OWNED"
    )
}

fn skip_filler(tokens: &[String], index: usize) -> usize {
    let mut next = index + 1;
    if tokens.get(next).is_some_and(|token| token.eq_ignore_ascii_case("BY") || token.eq_ignore_ascii_case("WITH")) {
        next += 1;
    }
    next
}

fn loose_tokens(sql: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < sql.len() {
        if let Some(next) = skip_delimited(sql, index) {
            index = next;
            continue;
        }
        let ch = sql[index..].chars().next().unwrap();
        if ch.is_whitespace() {
            index += ch.len_utf8();
            continue;
        }
        if ch.is_ascii_digit() || (ch == '-' && sql[index + ch.len_utf8()..].chars().next().is_some_and(|item| item.is_ascii_digit())) {
            let start = index;
            if ch == '-' {
                index += ch.len_utf8();
            }
            while index < sql.len() && sql[index..].chars().next().unwrap().is_ascii_digit() {
                index += 1;
            }
            tokens.push(sql[start..index].to_owned());
            continue;
        }
        if is_ident_start(ch) {
            let end = ident_continue(sql, index);
            tokens.push(sql[index..end].to_owned());
            index = end;
            continue;
        }
        index += ch.len_utf8();
    }
    tokens
}

fn sequence_alter_sql(engine: Engine, side: &Side, name: &str, spec: &SequenceSpec) -> String {
    let min_value = if spec.min_value.is_empty() { "NO MINVALUE".to_owned() } else { format!("MINVALUE {}", spec.min_value) };
    let max_value = if spec.max_value.is_empty() { "NO MAXVALUE".to_owned() } else { format!("MAXVALUE {}", spec.max_value) };
    let start = if spec.start.is_empty() { "1" } else { spec.start.as_str() };
    let cache = if spec.cache.is_empty() { "1" } else { spec.cache.as_str() };
    let cycle = if spec.cycle { "CYCLE" } else { "NO CYCLE" };
    format!(
        "ALTER SEQUENCE {} INCREMENT BY {} {min_value} {max_value} START WITH {start} CACHE {cache} {cycle}",
        qualify(engine, side, name),
        spec.increment
    )
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
    // PostgreSQL 的触发器名不能带 schema，schema 只写在 ON 后面的表上。
    let name = if object.kind == Kind::Trigger && engine == Engine::Postgres {
        quote(engine, &object.name)
    } else {
        qualify(engine, side, &object.name)
    };
    let mut sql = replace_name_after(definition, keyword, &name).unwrap_or_else(|| definition.to_owned());
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

fn dollar_delimiter_len(sql: &str, start: usize) -> Option<usize> {
    let rest = sql.get(start..)?;
    let tag_end = rest[1..].find('$')?;
    let delimiter = &rest[..=tag_end + 1];
    if !delimiter[1..delimiter.len() - 1].chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return None;
    }
    Some(delimiter.len())
}

fn skip_dollar(sql: &str, start: usize) -> Option<usize> {
    let delimiter_len = dollar_delimiter_len(sql, start)?;
    let delimiter = &sql[start..start + delimiter_len];
    let body = start + delimiter_len;
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

fn alter_table(
    engine: Engine,
    side: &Side,
    baseline_side: &Side,
    name: &str,
    baseline: &TableShape,
    target: &TableShape,
    with_fk: bool,
    with_pk: bool,
) -> Vec<String> {
    match engine {
        Engine::Sqlite => sqlite_alter(side, name, baseline, target, with_fk),
        Engine::Postgres => postgres_alter(side, baseline_side, name, baseline, target, with_pk),
        Engine::Mysql | Engine::Mariadb => mysql_alter(engine, side, name, baseline, target, with_pk),
    }
}

fn postgres_alter(side: &Side, baseline_side: &Side, name: &str, baseline: &TableShape, target: &TableShape, with_pk: bool) -> Vec<String> {
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
                    let using = enum_using(baseline_side, side, &other.type_name, &column.type_name, &column.name);
                    statements.push(format!(
                        "ALTER TABLE {table} ALTER COLUMN {} TYPE {}{using}",
                        quote(engine, &column.name),
                        column.type_name.trim()
                    ));
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

/// 两个枚举之间没有自动转型。经 text 转一次，标签对不上现有数据时由执行失败，而不是省略 USING。
fn enum_using(baseline: &Side, target: &Side, from_type: &str, to_type: &str, column: &str) -> String {
    if is_enum_type(target, from_type) && is_enum_type(baseline, to_type) {
        format!(" USING {}::text::{}", quote(Engine::Postgres, column), to_type.trim())
    } else {
        String::new()
    }
}

fn is_enum_type(side: &Side, type_name: &str) -> bool {
    side.objects.iter().any(|object| object.kind == Kind::Enum && type_mentions(type_name, &object.name))
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

fn table_choices(engine: Engine, baseline: &[ObjectDef], target: &[ObjectDef]) -> Vec<TableChoice> {
    let mut names = std::collections::BTreeSet::new();
    for object in baseline.iter().chain(target.iter()).filter(|object| object.kind == Kind::Table) {
        names.insert(object.name.clone());
    }
    names
        .into_iter()
        .map(|name| {
            let left = baseline.iter().find(|object| object.kind == Kind::Table && object.name == name);
            let right = target.iter().find(|object| object.kind == Kind::Table && object.name == name);
            let keys = table_data_keys(engine, left.and_then(|object| object.table.as_ref()), right.and_then(|object| object.table.as_ref())).unwrap_or_default();
            let unique = keys.iter().filter(|key| !key.primary).count();
            TableChoice {
                name,
                on_both: left.is_some() && right.is_some(),
                needs_key_choice: keys.iter().all(|key| !key.primary) && unique > 1,
                keys: keys.into_iter().map(key_choice).collect(),
            }
        })
        .collect()
}

fn key_choice(key: DataKey) -> KeyChoice {
    KeyChoice { name: key.name, columns: key.columns, primary: key.primary }
}

/// 索引没读到，且这一侧的列信息里没有主键时，键是未知而不是没有。
#[derive(Debug)]
pub struct KeysUnread;

/// 任一侧索引没读全，且该侧列信息没有主键时返回错误。
/// 非 SQLite 的主键不来自列信息，索引没读到即未知。
/// SQLite 两侧列上都有主键时仍按主键对比。
pub fn table_data_keys(engine: Engine, left: Option<&TableShape>, right: Option<&TableShape>) -> Result<Vec<DataKey>, KeysUnread> {
    for shape in left.into_iter().chain(right) {
        if shape.indexes_read {
            continue;
        }
        let column_primary = shape.columns.iter().any(|column| column.primary);
        if engine != Engine::Sqlite || !column_primary {
            return Err(KeysUnread);
        }
    }
    let Some(shape) = right.or(left) else {
        return Ok(Vec::new());
    };
    Ok(key_choices(shape)
        .into_iter()
        .map(|key| DataKey { name: key.name, columns: key.columns, primary: key.primary })
        .collect())
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
