//! 结构变更计划里给人看的文字（风险、阻断原因、步骤名）的英文版。
//!
//! 计划由执行层按中文生成，确认执行时还要和同一份计划逐条比对 SQL，所以不在生成端分语言；
//! 英文界面在显示时按这里的句型表换成英文。句型里的 `{}` 是对象名、行数等可变部分，
//! 英文句子用 `{0}`、`{1}` 按位置取回（可变部分本身是已知句型时也一起换）。
//! 覆盖范围：危险确认层（删除表/视图、清空、改名）与数据库管理页（VACUUM、REINDEX、PRAGMA、
//! ATTACH、外键开关、序列、事件）会出现的全部句子；查不到句型的原样返回（数据库原文不翻译）。

/// (中文句型, 英文句型)。同一前缀里更具体的句型排在前面。
const PATTERNS: &[(&str, &str)] = &[
    // 共用：对象核对、外键状态、限定名。
    ("预览的表和当前对象不一致，没有生成语句。", "The previewed table doesn't match the current object, so no statement was generated."),
    ("这不是基础表，这里不会删除它。", "This is not a base table, so it won't be dropped here."),
    ("找不到表 {}。", "Table {0} was not found."),
    ("目标表和已读取的结构不一致，没有生成语句。", "The target table doesn't match the structure that was read, so no statement was generated."),
    ("这是视图。首版只查看视图，不生成结构变更。", "This is a view. Views are read-only here, so no structure change is generated."),
    ("找不到视图 {}。", "View {0} was not found."),
    ("目标视图和已读取的对象不一致，没有生成语句。", "The target view doesn't match the object that was read, so no statement was generated."),
    ("这是基础表，不是视图，这里不会删除它。", "This is a base table, not a view, so it won't be dropped here."),
    (
        "当前连接 foreign_keys = {}。不会为了让这次操作看起来成功而静默打开或关闭它。",
        "This connection has foreign_keys = {0}. It is never switched on or off silently to make this operation look successful.",
    ),
    ("未知", "unknown"),
    ("Redis 没有结构编辑。", "Redis has no structure editing."),
    ("MongoDB 没有结构编辑。", "MongoDB has no structure editing."),
    ("当前连接打开的是 main，没有另开连接修改 {}", "This connection has main open; no other connection was opened to change {0}"),
    ("需要数据库名，没有另开连接。", "A database name is required; no other connection was opened."),
    ("PostgreSQL 需要指定 schema（表所在的分组名），没有另开连接。", "PostgreSQL needs a schema (the group the table is in); no other connection was opened."),
    ("SQL Server 需要指定 schema（表所在的分组名），没有另开连接。", "SQL Server needs a schema (the group the table is in); no other connection was opened."),
    ("被这些外键引用：{}。{}", "Referenced by these foreign keys: {0}. {1}"),
    ("约 {} 行", "about {0} rows"),
    // 删除表
    ("现有数据", "its existing data"),
    ("将永久删除 {} 的结构和{}。这不是备份。", "This permanently deletes the structure of {0} and {1}. This is not a backup."),
    ("只删除预览中的这一张表，不隐式级联删除其他业务对象。", "Only this previewed table is dropped; no other object is removed by an implicit cascade."),
    (
        "不会添加 CASCADE。当前 foreign_keys = OFF 时 SQLite 仍可能删除这张表，引用方会留下无效引用。",
        "CASCADE is not added. With foreign_keys = OFF, SQLite may still drop this table and leave dangling references behind.",
    ),
    ("不会添加 CASCADE。被引用时数据库会拒绝删除。", "CASCADE is not added. The database refuses the drop while the table is referenced."),
    ("删除视图 {}", "Drop view {0}"),
    ("删除 {}", "Drop {0}"),
    // 删除视图
    ("将永久删除视图 {} 的定义。引用它的查询、视图和程序会失败。", "This permanently deletes the definition of view {0}. Queries, views and programs that use it will fail."),
    (
        "只删除预览中的这一个视图，不删除底层表的数据，也不加 CASCADE。",
        "Only this previewed view is dropped; the data in the underlying tables stays and CASCADE is not added.",
    ),
    ("这是物化视图，已经物化的数据会一起删除。", "This is a materialized view; its materialized data is deleted with it."),
    ("这些视图可能引用它：{}。数据库可能拒绝删除，或让它们失效。", "These views may reference it: {0}. The database may refuse the drop or leave them invalid."),
    // 清空表
    ("全部", "all"),
    (
        "将永久删除 {} 里的{}数据，只保留表结构。这不是备份，也不能撤销。",
        "This permanently deletes {1} of the data in {0} and keeps only the table structure. This is not a backup and can't be undone.",
    ),
    (
        "SQLite 没有 TRUNCATE，这里用 DELETE FROM 逐行删除；AUTOINCREMENT 的计数不会重置，删除触发器会触发。",
        "SQLite has no TRUNCATE, so DELETE FROM removes the rows one by one; the AUTOINCREMENT counter is not reset and delete triggers fire.",
    ),
    (
        "TRUNCATE TABLE 会立即提交并把 AUTO_INCREMENT 重置到初始值，不触发 DELETE 触发器。",
        "TRUNCATE TABLE commits immediately and resets AUTO_INCREMENT to its initial value; DELETE triggers don't fire.",
    ),
    (
        "TRUNCATE TABLE 不触发逐行的 DELETE 触发器，不重置关联序列，也不加 CASCADE。",
        "TRUNCATE TABLE doesn't fire row-level DELETE triggers, doesn't reset owned sequences and doesn't add CASCADE.",
    ),
    ("TRUNCATE TABLE 会重置 IDENTITY 计数，不触发 DELETE 触发器。", "TRUNCATE TABLE resets the IDENTITY counter; DELETE triggers don't fire."),
    (
        "当前 foreign_keys = ON：引用方还有数据时删除可能被拒绝，或按外键规则级联。",
        "foreign_keys is ON: while referencing rows exist, the delete may be refused or cascade by the foreign-key rules.",
    ),
    ("当前 foreign_keys 未开启：删除会成功，引用方会留下无效引用。", "foreign_keys is off: the delete succeeds and the referencing rows are left with dangling references."),
    ("被外键引用的表，数据库可能拒绝 TRUNCATE。不会添加 CASCADE。", "The database may refuse TRUNCATE on a table referenced by foreign keys. CASCADE is not added."),
    ("清空 {}", "Empty {0}"),
    // 改名
    ("新表名不能为空。", "The new table name can't be empty."),
    ("新表名无效。", "The new table name is not valid."),
    ("新表名和原名相同，没有生成语句。", "The new name is the same as the current one, so no statement was generated."),
    (
        "SQLite 不能只改表名的大小写（{} → {}）。可以先改成另一个临时名，再改成 {}。",
        "SQLite can't change only the letter case of a table name ({0} → {1}). Rename it to a temporary name first, then to {2}.",
    ),
    ("名字 {} 已经被其它对象占用，不会覆盖它。", "The name {0} is already used by another object; it won't be overwritten."),
    ("没有确认名字 {} 是否已被占用，没有生成语句。", "It couldn't be confirmed whether the name {0} is free, so no statement was generated."),
    (
        "将把 {} 改名为 {}。引用旧名的查询、视图、存储过程和应用代码可能失败。",
        "This renames {0} to {1}. Queries, views, stored procedures and application code that use the old name may fail.",
    ),
    ("这些视图或触发器引用了它：{}。数据库不一定会同步改写它们。", "These views or triggers reference it: {0}. The database won't necessarily update them."),
    ("sp_rename 不会改写引用旧名的存储过程、函数和视图。", "sp_rename doesn't update stored procedures, functions or views that use the old name."),
    ("把 {} 改名为 {}", "Rename {0} to {1}"),
    // 外键开关
    (
        "只改变本次已打开连接的 foreign_keys。不会清理已有数据，也不会把外键说成已经验证成功。",
        "Only changes foreign_keys on this open connection. Existing data is not cleaned up, and the foreign keys are not reported as verified.",
    ),
    ("只有 SQLite 用连接上的 foreign_keys 控制外键是否生效。", "Only SQLite uses the connection's foreign_keys setting to enforce foreign keys."),
    ("没有读到 foreign_keys，没有生成语句。", "foreign_keys could not be read, so no statement was generated."),
    ("当前连接 foreign_keys 已经是 {}。", "foreign_keys is already {0} on this connection."),
    ("设置 foreign_keys = {}", "Set foreign_keys = {0}"),
    // PRAGMA
    ("只有 SQLite 用 PRAGMA 改这些开关，这里不生成语句。", "Only SQLite changes these settings with PRAGMA, so no statement is generated here."),
    (
        "foreign_keys 走外键开关动作（SetForeignKeys），这里不重复生成同一条 PRAGMA。",
        "foreign_keys has its own action (SetForeignKeys), so the same PRAGMA is not generated here again.",
    ),
    ("journal_mode 只能是 delete、truncate、persist、memory、wal 或 off。", "journal_mode must be delete, truncate, persist, memory, wal or off."),
    ("journal_mode 写进数据库文件头，重新打开这个文件后仍然是这个模式。", "journal_mode is written to the database file header and stays in effect when the file is reopened."),
    (
        "WAL 会在库文件旁边多出 -wal 与 -shm 文件，需要文件系统支持共享内存。",
        "WAL adds -wal and -shm files next to the database file and needs a file system that supports shared memory.",
    ),
    ("关掉日志后崩溃时无法回滚未完成的事务。", "With the journal off, unfinished transactions can't be rolled back after a crash."),
    ("user_version 只接受 0 到 2147483647 的整数。", "user_version only accepts an integer from 0 to 2147483647."),
    ("这里只改 journal_mode 与 user_version，其它 PRAGMA 不放行。", "Only journal_mode and user_version can be changed here; other PRAGMAs are not allowed."),
    ("{} 已经是 {}，没有生成语句。", "{0} is already {1}, so no statement was generated."),
    ("设置 {} = {}", "Set {0} = {1}"),
    // ATTACH
    ("只有 SQLite 用 ATTACH 附加数据库文件，这里不生成语句。", "Only SQLite attaches database files with ATTACH, so no statement is generated here."),
    ("要附加的数据库文件路径不能为空。", "The path of the database file to attach can't be empty."),
    ("文件路径里不能有控制字符。", "The file path can't contain control characters."),
    ("文件路径太长。", "The file path is too long."),
    ("附加库的名字无效。", "The name for the attached database is not valid."),
    ("{} 是 SQLite 保留的库名，不能用来附加。", "{0} is a database name reserved by SQLite and can't be used for an attachment."),
    ("{} 已经附加在这个连接上，没有生成语句。", "{0} is already attached on this connection, so no statement was generated."),
    (
        "路径上没有库文件时 SQLite 会新建一个空库；这里不检查文件是否已经存在。",
        "If there is no database file at the path, SQLite creates an empty one; whether the file exists is not checked here.",
    ),
    ("ATTACH 只对当前连接生效，断开连接后不保留。", "ATTACH only applies to this connection and is gone after disconnecting."),
    ("同一个文件已经作为 {} 附加过；再加一次会在写入时互相干扰。", "The same file is already attached as {0}; attaching it again makes writes interfere with each other."),
    ("附加 {}", "Attach {0}"),
    // VACUUM / REINDEX
    (
        "VACUUM 会重写整个数据库文件，需要额外的临时磁盘空间；不确认执行就不会写入。",
        "VACUUM rewrites the whole database file and needs extra temporary disk space; nothing is written until you confirm.",
    ),
    (
        "VACUUM 不能在事务里执行，这里单条执行，不改表结构也不改数据。",
        "VACUUM can't run inside a transaction, so it runs on its own; it changes neither the structure nor the data.",
    ),
    ("维护动作只对 SQLite 提供；其它引擎请在查询里自己写 VACUUM。", "Maintenance actions are only offered for SQLite; for other engines, write VACUUM in a query."),
    ("整理数据库文件", "Compact the database file"),
    ("REINDEX 只重建索引，不改表里的数据。", "REINDEX only rebuilds indexes; table data is not changed."),
    ("大表重建索引期间会占用写锁，其它写入要等它结束。", "Rebuilding the indexes of a large table holds a write lock; other writes wait until it finishes."),
    ("维护动作只对 SQLite 提供；其它引擎请在查询里自己写 REINDEX。", "Maintenance actions are only offered for SQLite; for other engines, write REINDEX in a query."),
    ("要重建的索引或表名无效。", "The index or table name to rebuild is not valid."),
    ("重建当前库的索引", "Rebuild the indexes of the current database"),
    ("重建 {} 的索引", "Rebuild the indexes of {0}"),
    // 序列
    (
        "序列参数改完立即生效，之后取号按新参数生成。已有数据里的号不会因此改变。",
        "Sequence settings take effect immediately and new values follow them. Values already in the data don't change.",
    ),
    (
        "只有 PostgreSQL 的序列参数由这里修改；其它引擎请在查询里自己写 ALTER SEQUENCE。",
        "Only PostgreSQL sequence settings are changed here; for other engines, write ALTER SEQUENCE in a query.",
    ),
    ("序列名无效。", "The sequence name is not valid."),
    ("没有读到序列 {} 的参数，没有生成语句。", "The settings of sequence {0} could not be read, so no statement was generated."),
    (
        "序列 {} 的参数读不到（可能是权限不足），没有生成语句。",
        "The settings of sequence {0} can't be read (possibly a missing privilege), so no statement was generated.",
    ),
    ("步长不能是 0。", "The increment can't be 0."),
    ("最小值必须小于最大值。", "The minimum must be less than the maximum."),
    (
        "START WITH 只改以后 RESTART 用的起点，不会移动已经取到的号；要移动当前号得另外 RESTART。",
        "START WITH only changes the starting point used by a later RESTART and doesn't move the current value; moving it needs a separate RESTART.",
    ),
    (
        "调小上界或调大下界可能让之后的取号失败；这里不检查已经用掉的号。",
        "Lowering the maximum or raising the minimum may make later values fail; values already used are not checked here.",
    ),
    ("序列 {} 的参数没有变化。", "The settings of sequence {0} have not changed."),
    ("修改序列 {}", "Alter sequence {0}"),
    // 执行规则（管理页预览里显示）
    (
        "这些语句逐条执行，失败即停。MySQL 与 MariaDB 会立即提交每条结构变更语句，中途失败不会撤销已经成功的语句，不能声称已经全部回滚。",
        "These statements run one at a time and stop at the first failure. MySQL and MariaDB commit each structure change immediately, so a failure midway doesn't undo the statements that already succeeded; nothing can be claimed as fully rolled back.",
    ),
    (
        "这些语句逐条执行，失败即停。已经成功的语句不撤销。PostgreSQL 虽然可以把结构变更放进事务，这里也不包成一个大事务，不能据此认为所有数据库都会回滚。",
        "These statements run one at a time and stop at the first failure. Statements that already succeeded are not undone. PostgreSQL could put structure changes in a transaction, but they are not wrapped in one here, so don't assume every database rolls back.",
    ),
    ("这些语句逐条执行，失败即停。已经成功的语句不撤销，后面的语句不执行。", "These statements run one at a time and stop at the first failure. Statements that already succeeded are not undone, and the rest don't run."),
    (
        "这些语句逐条执行，失败即停。已成功的语句不撤销；这里也不把整批包进一个事务，不能据此声称可以整体回滚。",
        "These statements run one at a time and stop at the first failure. Statements that already succeeded are not undone, and the batch is not wrapped in one transaction, so it can't be claimed to roll back as a whole.",
    ),
    // 事件
    ("只改事件的启停状态，不改它的事件体。", "Only the event's enabled state changes; its body stays the same."),
    ("只有 MySQL 与 MariaDB 有事件（event）对象，这里不生成语句。", "Only MySQL and MariaDB have event objects, so no statement is generated here."),
    ("事件名无效。", "The event name is not valid."),
    ("没有读到事件 {}，没有生成语句。", "Event {0} could not be read, so no statement was generated."),
    ("事件 {} 已经是 {}。", "Event {0} is already {1}."),
    ("event_scheduler 关闭时事件仍然不会执行；这里不改调度器状态。", "Events still don't run while event_scheduler is off; the scheduler state is not changed here."),
    ("启用事件 {}", "Enable event {0}"),
    ("停用事件 {}", "Disable event {0}"),
];

/// 是否含中日韩文字或全角标点。
pub fn has_cjk(text: &str) -> bool {
    text.chars().any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch) || ('\u{3000}'..='\u{303f}').contains(&ch) || ('\u{ff00}'..='\u{ffef}').contains(&ch))
}

/// 按句型把 `text` 拆成可变部分（可以为空，例如还没填的新名字）；不匹配返回 None。
/// 中间的可变部分取最短匹配，最后一段吃到结尾。
fn captures(pattern: &str, text: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = pattern.split("{}").collect();
    let mut rest = text.strip_prefix(parts[0])?;
    if parts.len() == 1 {
        return rest.is_empty().then(Vec::new);
    }
    let mut values = Vec::new();
    for (index, literal) in parts.iter().enumerate().skip(1) {
        if index == parts.len() - 1 {
            values.push(rest.strip_suffix(literal)?.to_owned());
        } else {
            if literal.is_empty() {
                return None;
            }
            let at = rest.find(literal)?;
            values.push(rest[..at].to_owned());
            rest = &rest[at + literal.len()..];
        }
    }
    Some(values)
}

/// 可变部分：本身是已知句子就换成英文；名字列表里的顿号换成逗号。
fn value_in_english(value: &str) -> String {
    if !has_cjk(value) {
        return value.to_owned();
    }
    translate(value).unwrap_or_else(|| value.replace('、', ", "))
}

fn translate(text: &str) -> Option<String> {
    PATTERNS.iter().find_map(|(zh, en)| {
        let values = captures(zh, text)?;
        let mut out = (*en).to_owned();
        for (index, value) in values.iter().enumerate() {
            out = out.replace(&format!("{{{index}}}"), &value_in_english(value));
        }
        Some(out)
    })
}

/// 计划文字的英文版；不是已知句型时原样返回。
pub fn english(text: &str) -> String {
    if !has_cjk(text) {
        return text.to_owned();
    }
    translate(text).unwrap_or_else(|| text.to_owned())
}

/// 按界面语言给出计划文字。
pub fn localize(text: &str, locale: crate::i18n::Locale) -> String {
    match locale {
        crate::i18n::Locale::En => english(text),
        crate::i18n::Locale::ZhCn => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::{Action, AttachedDatabase, DependentSql, Engine, IncomingForeignKey, Plan, SequenceState, Snapshot};
    use super::super::plan::plan;
    use super::*;

    fn snapshot(engine: Engine, kind: &str, name: &str) -> Snapshot {
        let database = if engine == Engine::Sqlite { "main" } else { "shop" };
        let schema = if matches!(engine, Engine::Postgres | Engine::Sqlserver) { "public" } else { "" };
        let mut snapshot = Snapshot::bare(engine, database, schema, name);
        snapshot.kind = kind.to_owned();
        snapshot
    }

    fn assert_english(plan: &Plan, case: &str) {
        let mut all: Vec<String> = plan.risks.iter().map(|risk| risk.text.clone()).collect();
        all.extend(plan.blocked.iter().cloned());
        all.extend(plan.steps.iter().map(|step| step.label.clone()));
        assert!(!all.is_empty(), "{case}: 计划没有文字");
        all.push(plan.rule.clone());
        for text in all {
            let en = english(&text);
            assert!(!has_cjk(&en), "{case}: 没有英文句型：{text} -> {en}");
        }
    }

    fn incoming(table: &str, name: &str) -> IncomingForeignKey {
        IncomingForeignKey { name: name.to_owned(), table: table.to_owned(), schema: String::new(), columns: Vec::new(), ref_columns: Vec::new() }
    }

    /// 危险确认层与管理页在各引擎、各分支下生成的每一句话都有英文；对象名原样保留。
    #[test]
    fn every_danger_and_admin_plan_sentence_has_an_english_form() {
        for engine in [Engine::Sqlite, Engine::Postgres, Engine::Mysql, Engine::Mariadb, Engine::Sqlserver] {
            for (dependents, rows, referenced, live) in [(false, None, false, None), (true, Some(180), true, Some(true)), (true, Some(3), true, Some(false))] {
                let mut table = snapshot(engine, "table", "orders");
                table.row_count = rows;
                table.live_foreign_keys = live;
                if referenced {
                    table.incoming = vec![incoming("lines", "fk_lines_order"), incoming("refunds", "fk_refund")];
                }
                if dependents {
                    table.views = vec![DependentSql { name: "order_view".into(), sql: String::new() }];
                    table.triggers = vec![DependentSql { name: "order_audit".into(), sql: String::new() }];
                }
                for action in [
                    Action::DropTable { table: "orders".into() },
                    Action::TruncateTable { table: "orders".into() },
                    Action::RenameTable { table: "orders".into(), new_name: "orders_old".into() },
                    Action::RenameTable { table: "orders".into(), new_name: String::new() },
                    Action::RenameTable { table: "orders".into(), new_name: "orders".into() },
                    Action::RenameTable { table: "orders".into(), new_name: "ORDERS".into() },
                    Action::RenameTable { table: "orders".into(), new_name: "bad\u{1}".into() },
                ] {
                    for taken in [None, Some(true), Some(false)] {
                        table.name_taken = taken;
                        assert_english(&plan(&table, &action), &format!("{engine:?} {action:?} {taken:?}"));
                    }
                }
                let mut view = snapshot(engine, "view", "order_view");
                view.materialized = dependents;
                view.views = table.views.clone();
                view.views.push(DependentSql { name: "summary_view".into(), sql: String::new() });
                assert_english(&plan(&view, &Action::DropView { view: "order_view".into() }), &format!("{engine:?} drop view"));
                assert_english(&plan(&view, &Action::DropTable { table: "order_view".into() }), &format!("{engine:?} drop table on view"));
                assert_english(&plan(&view, &Action::TruncateTable { table: "order_view".into() }), &format!("{engine:?} truncate view"));
                assert_english(&plan(&table, &Action::DropView { view: "orders".into() }), &format!("{engine:?} drop view on table"));
                assert_english(&plan(&view, &Action::DropView { view: "other".into() }), &format!("{engine:?} view mismatch"));
                let missing = snapshot(engine, "missing", "gone");
                assert_english(&plan(&missing, &Action::TruncateTable { table: "gone".into() }), &format!("{engine:?} missing"));
                assert_english(&plan(&missing, &Action::DropView { view: "gone".into() }), &format!("{engine:?} missing view"));
                assert_english(&plan(&table, &Action::DropTable { table: "other".into() }), &format!("{engine:?} mismatch"));
                assert_english(&plan(&table, &Action::TruncateTable { table: "other".into() }), &format!("{engine:?} truncate mismatch"));
            }
            // 管理页动作
            let mut db = snapshot(engine, "database", "main");
            for live in [None, Some(true), Some(false)] {
                db.live_foreign_keys = live;
                for enabled in [true, false] {
                    assert_english(&plan(&db, &Action::SetForeignKeys { enabled }), &format!("{engine:?} fk {live:?} {enabled}"));
                }
            }
            db.pragmas.insert("user_version".into(), "3".into());
            db.attached = vec![AttachedDatabase { name: "extra".into(), path: "/tmp/extra.sqlite".into() }];
            for (pragma, value) in [
                ("journal_mode", "wal"),
                ("journal_mode", "off"),
                ("journal_mode", "bogus"),
                ("user_version", "3"),
                ("user_version", "-1"),
                ("user_version", "7"),
                ("cache_size", "1"),
                ("foreign_keys", "on"),
            ] {
                assert_english(&plan(&db, &Action::SetPragma { pragma: pragma.into(), value: value.into() }), &format!("{engine:?} pragma {pragma}={value}"));
            }
            for (path, name) in [("/tmp/other.sqlite", "other"), ("", "bad\u{1}"), ("/tmp/extra.sqlite", "main"), ("/tmp/extra.sqlite", "extra"), ("/tmp/a\u{1}b", "x")] {
                assert_english(&plan(&db, &Action::AttachDatabase { path: path.into(), name: name.into() }), &format!("{engine:?} attach {name}"));
            }
            assert_english(&plan(&db, &Action::AttachDatabase { path: "x".repeat(1100), name: "long".into() }), &format!("{engine:?} attach long"));
            assert_english(&plan(&db, &Action::Vacuum), &format!("{engine:?} vacuum"));
            for target in [None, Some("orders".to_owned()), Some("bad\u{1}".to_owned())] {
                assert_english(&plan(&db, &Action::Reindex { target }), &format!("{engine:?} reindex"));
            }
            let mut sequence = snapshot(engine, "sequence", "orders_id_seq");
            for current in [None, Some(SequenceState::default()), Some(SequenceState { start: Some(1), increment: Some(1), min_value: Some(1), max_value: Some(100), cycle: Some(false) })] {
                sequence.sequence = current;
                for wanted in [
                    SequenceState { increment: Some(0), ..Default::default() },
                    SequenceState { start: Some(5), min_value: Some(10), max_value: Some(5), ..Default::default() },
                    SequenceState { increment: Some(1), ..Default::default() },
                    SequenceState { increment: Some(2), max_value: Some(50), ..Default::default() },
                ] {
                    let action = Action::AlterSequence {
                        sequence: "orders_id_seq".into(),
                        start: wanted.start,
                        increment: wanted.increment,
                        min_value: wanted.min_value,
                        max_value: wanted.max_value,
                        cycle: wanted.cycle,
                    };
                    assert_english(&plan(&sequence, &action), &format!("{engine:?} sequence"));
                }
            }
            let mut event = snapshot(engine, "event", "nightly");
            for current in [None, Some(true), Some(false)] {
                event.event_enabled = current;
                for enabled in [true, false] {
                    assert_english(&plan(&event, &Action::SetEventEnabled { event: "nightly".into(), enabled }), &format!("{engine:?} event"));
                }
            }
        }
        for engine in [Engine::Redis, Engine::Mongodb] {
            let mut table = snapshot(engine, "table", "orders");
            table.name_taken = Some(false);
            assert_english(&plan(&table, &Action::RenameTable { table: "orders".into(), new_name: "x".into() }), &format!("{engine:?} rename"));
        }
    }

    #[test]
    fn english_keeps_names_and_passes_unknown_text_through() {
        assert_eq!(english("删除 orders"), "Drop orders");
        assert_eq!(
            english("将把 sqlite / orders 改名为 orders_old。引用旧名的查询、视图、存储过程和应用代码可能失败。"),
            "This renames sqlite / orders to orders_old. Queries, views, stored procedures and application code that use the old name may fail."
        );
        assert_eq!(
            english("将永久删除 sqlite / orders 的结构和约 180 行。这不是备份。"),
            "This permanently deletes the structure of sqlite / orders and about 180 rows. This is not a backup."
        );
        assert_eq!(
            english("被这些外键引用：lines.fk_a、refunds.fk_b。不会添加 CASCADE。被引用时数据库会拒绝删除。"),
            "Referenced by these foreign keys: lines.fk_a, refunds.fk_b. CASCADE is not added. The database refuses the drop while the table is referenced."
        );
        assert_eq!(english("no such table: orders"), "no such table: orders", "数据库原文不改");
        assert_eq!(english("一句没有句型的话"), "一句没有句型的话");
        assert_eq!(localize("删除 orders", crate::i18n::Locale::ZhCn), "删除 orders");
    }
}
