//! SQLite 专属说明句的英文：计划风险/阻断/步骤、读取与执行的错误、对象定义提示等只由这个引擎生成的中文句子。
//! 由 `EngineMeta::messages_en` 交给 `ddl::english`，先于共用句型查找；`{}` 等占位规则见 `ddl/english.rs`。

/// (中文句型, 英文句型)。同一前缀里更具体的句型排在前面。
pub(in crate::adapters) const MESSAGES_EN: &[(&str, &str)] = &[
    (
        "WITHOUT ROWID 表必须保留主键，重建不能可靠保留行标识，因此阻止执行。",
        "A WITHOUT ROWID table must keep its primary key and a rebuild can't keep the row identity reliably, so it is blocked.",
    ),
    ("只有 SQLite 用 PRAGMA 改这些开关，这里不生成语句。", "Only SQLite changes these settings with PRAGMA, so no statement is generated here."),
    (
        "foreign_keys 走外键开关动作（SetForeignKeys），这里不重复生成同一条 PRAGMA。",
        "foreign_keys has its own action (SetForeignKeys), so the same PRAGMA is not generated here again.",
    ),
    ("这里只改 journal_mode 与 user_version，其它 PRAGMA 不放行。", "Only journal_mode and user_version can be changed here; other PRAGMAs are not allowed."),
    (
        "不会添加 CASCADE。当前 foreign_keys = OFF 时 SQLite 仍可能删除这张表，引用方会留下无效引用。",
        "CASCADE is not added. With foreign_keys = OFF, SQLite may still drop this table and leave dangling references behind.",
    ),
    (
        "SQLite 没有 TRUNCATE，这里用 DELETE FROM 逐行删除；AUTOINCREMENT 的计数不会重置，删除触发器会触发。",
        "SQLite has no TRUNCATE, so DELETE FROM removes the rows one by one; the AUTOINCREMENT counter is not reset and delete triggers fire.",
    ),
    (
        "当前 foreign_keys = ON：引用方还有数据时删除可能被拒绝，或按外键规则级联。",
        "foreign_keys is ON: while referencing rows exist, the delete may be refused or cascade by the foreign-key rules.",
    ),
    ("当前 foreign_keys 未开启：删除会成功，引用方会留下无效引用。", "foreign_keys is off: the delete succeeds and the referencing rows are left with dangling references."),
    (
        "SQLite 不能只改表名的大小写（{} → {}）。可以先改成另一个临时名，再改成 {}。",
        "SQLite can't change only the letter case of a table name ({0} → {1}). Rename it to a temporary name first, then to {2}.",
    ),
    (
        "只改变本次已打开连接的 foreign_keys。不会清理已有数据，也不会把外键说成已经验证成功。",
        "Only changes foreign_keys on this open connection. Existing data is not cleaned up, and the foreign keys are not reported as verified.",
    ),
    ("只有 SQLite 用连接上的 foreign_keys 控制外键是否生效。", "Only SQLite uses the connection's foreign_keys setting to enforce foreign keys."),
    ("没有读到 foreign_keys，没有生成语句。", "foreign_keys could not be read, so no statement was generated."),
    ("当前连接 foreign_keys 已经是 {}。", "foreign_keys is already {0} on this connection."),
    ("设置 foreign_keys = {}", "Set foreign_keys = {0}"),
    ("journal_mode 只能是 delete、truncate、persist、memory、wal 或 off。", "journal_mode must be delete, truncate, persist, memory, wal or off."),
    ("journal_mode 写进数据库文件头，重新打开这个文件后仍然是这个模式。", "journal_mode is written to the database file header and stays in effect when the file is reopened."),
    (
        "WAL 会在库文件旁边多出 -wal 与 -shm 文件，需要文件系统支持共享内存。",
        "WAL adds -wal and -shm files next to the database file and needs a file system that supports shared memory.",
    ),
    ("关掉日志后崩溃时无法回滚未完成的事务。", "With the journal off, unfinished transactions can't be rolled back after a crash."),
    ("user_version 只接受 0 到 2147483647 的整数。", "user_version only accepts an integer from 0 to 2147483647."),
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
    (
        "VACUUM 会重写整个数据库文件，需要额外的临时磁盘空间；不确认执行就不会写入。",
        "VACUUM rewrites the whole database file and needs extra temporary disk space; nothing is written until you confirm.",
    ),
    (
        "VACUUM 不能在事务里执行，这里单条执行，不改表结构也不改数据。",
        "VACUUM can't run inside a transaction, so it runs on its own; it changes neither the structure nor the data.",
    ),
    ("维护动作只对 SQLite 提供；其它引擎请在查询里自己写 VACUUM。", "Maintenance actions are only offered for SQLite; for other engines, write VACUUM in a query."),
    ("REINDEX 只重建索引，不改表里的数据。", "REINDEX only rebuilds indexes; table data is not changed."),
    ("大表重建索引期间会占用写锁，其它写入要等它结束。", "Rebuilding the indexes of a large table holds a write lock; other writes wait until it finishes."),
    ("维护动作只对 SQLite 提供；其它引擎请在查询里自己写 REINDEX。", "Maintenance actions are only offered for SQLite; for other engines, write REINDEX in a query."),
    ("要重建的索引或表名无效。", "The index or table name to rebuild is not valid."),
    ("重建当前库的索引", "Rebuild the indexes of the current database"),
    (
        "这些语句逐条执行，失败即停。已经成功的语句不撤销，后面的语句不执行。",
        "These statements run one at a time and stop at the first failure. Statements that already succeeded are not undone, and the rest don't run.",
    ),
    ("元数据是空的", "The metadata is empty"),
    ("由唯一约束维护", "Maintained by a unique constraint"),
    ("VACUUM 已完成：数据库现在有 {} 页，其中空闲 {} 页。", "VACUUM finished: the database now has {0} pages, {1} of them free."),
    ("VACUUM 已完成。", "VACUUM finished."),
    ("REINDEX 已完成：索引已按当前的表结构重建。", "REINDEX finished: the indexes were rebuilt for the current table structure."),
    (
        "SQLite 不能直接 ALTER TABLE 添加外键，需要按官方步骤重建表。",
        "SQLite can't add a foreign key with ALTER TABLE; the table has to be rebuilt by the official steps.",
    ),
    ("SQLite 需要重建表才能移除外键。", "SQLite has to rebuild the table to remove a foreign key."),
    ("SQLite 新增非空字段 {} 必须有非 NULL 默认值。", "In SQLite, the new NOT NULL column {0} needs a non-NULL default."),
    ("去掉 NOT NULL {}", "Drop NOT NULL on {0}"),
    (
        "需要重建表。SQLite 不能直接完成这项修改，将按官方 12 步新建表、复制数据并替换原表。",
        "The table needs a rebuild. SQLite can't make this change directly, so the official 12 steps create a new table, copy the data and replace the original.",
    ),
    ("索引 {} 引用了被删除或无法原样保留的列，重建时不会保留它。", "Index {0} uses a column that is dropped or can't be kept as is, so the rebuild won't keep it."),
    ("没有可以可靠复制的原有列，因此阻止重建。", "No original column can be copied reliably, so the rebuild is blocked."),
    ("复制保留列的数据", "Copy the data of the kept columns"),
    (
        "不知道本次会话原来的 foreign_keys 设置，不能在重建后恢复，因此没有执行。",
        "The session's original foreign_keys setting is unknown and couldn't be restored after the rebuild, so nothing was run.",
    ),
    ("在事务外关闭外键检查", "Turn off foreign key checks outside the transaction"),
    ("开始重建事务", "Begin the rebuild transaction"),
    ("按新定义创建新表", "Create the new table from the new definition"),
    ("删除旧表", "Drop the old table"),
    ("新表改回原名", "Rename the new table to the original name"),
    ("重建索引 {}", "Rebuild index {0}"),
    ("重建触发器 {}", "Rebuild trigger {0}"),
    ("检查外键；有违规则停止并回滚这次重建", "Check foreign keys; stop and roll back this rebuild on a violation"),
    ("提交重建事务", "Commit the rebuild transaction"),
    ("恢复会话原来的 foreign_keys = {}", "Restore the session's original foreign_keys = {0}"),
    ("恢复会话原来的 foreign_keys", "Restore the session's original foreign_keys"),
    (
        "当前连接 foreign_keys = OFF。重建结束后仍恢复为 OFF，外键定义不会因此变成强制检查。",
        "This connection has foreign_keys = OFF. It is set back to OFF after the rebuild, so the foreign key definitions are not enforced because of it.",
    ),
    (
        "读不到原来的建表语句，不能确认索引和约束能否保留，因此阻止重建。",
        "The original CREATE TABLE statement couldn't be read, so it can't be confirmed that indexes and constraints are kept; the rebuild is blocked.",
    ),
    ("表含生成列，重建不能可靠保留这些列，因此阻止执行。", "The table has generated columns that a rebuild can't keep reliably, so it is blocked."),
    ("表含 CHECK 约束，重建不能可靠保留该约束，因此阻止执行。", "The table has a CHECK constraint that a rebuild can't keep reliably, so it is blocked."),
    ("表含排序规则，重建不能可靠保留该规则，因此阻止执行。", "The table has a collation that a rebuild can't keep reliably, so it is blocked."),
    ("触发器 {} 引用了将改变的列，无法可靠改写，因此阻止重建。", "Trigger {0} uses columns that will change and can't be rewritten reliably, so the rebuild is blocked."),
    ("读不到触发器 {} 的定义，不能可靠重建。", "The definition of trigger {0} couldn't be read, so it can't be rebuilt reliably."),
    ("视图 {} 引用了将改变的结构，无法可靠改写，因此阻止重建。", "View {0} uses structure that will change and can't be rewritten reliably, so the rebuild is blocked."),
    ("外键 {} 引用了将改变的列，重建不能可靠保留该约束。", "Foreign key {0} uses columns that will change, so the rebuild can't keep that constraint reliably."),
    ("索引 {} 含表达式或部分条件，列被重命名后无法可靠改写。", "Index {0} has an expression or partial condition and can't be rewritten reliably after a column rename."),
    ("读不到索引 {} 的原始语句，不能可靠保留。", "The original statement of index {0} couldn't be read, so it can't be kept reliably."),
    ("表达式索引 {} 在列重命名后无法可靠改写。", "Expression index {0} can't be rewritten reliably after a column rename."),
    (
        "SQLite 按官方步骤重建：先在事务外关闭外键检查，再在一个事务里建新表、复制数据、删除旧表并改回原名，然后做 foreign_key_check（外键完整性检查）。这只属于这次重建，不是所有数据库都能回滚。事务内失败会回滚这次重建；事务外改过的 foreign_keys 会恢复为会话原来的值。",
        "SQLite rebuilds by the official steps: foreign key checks are turned off outside the transaction, then one transaction creates the new table, copies the data, drops the old table and renames the new one back, followed by foreign_key_check (a foreign key integrity check). This applies to this rebuild only; not every database can roll back. A failure inside the transaction rolls this rebuild back; foreign_keys changed outside the transaction is restored to the session's original value.",
    ),
    ("重建事务已回滚，这条语句没有保留。", "The rebuild transaction was rolled back, so this statement was not kept."),
    ("回滚重建事务失败，结果未知：{}", "Rolling back the rebuild transaction failed, so the result is unknown: {0}"),
    ("已恢复会话原来的 foreign_keys。", "The session's original foreign_keys was restored."),
    ("恢复 foreign_keys 失败，结果未知：{}", "Restoring foreign_keys failed, so the result is unknown: {0}"),
    ("数据库返回：{}", "The database returned: {0}"),
    ("foreign_key_check 发现 {} 处违规，不会把这次重建当成成功。", "foreign_key_check found {0} violations, so this rebuild is not treated as a success."),
    ("SQLite 没有函数或存储过程", "SQLite has no functions or stored procedures"),
    ("SQLite 没有序列、枚举或域", "SQLite has no sequences, enums or domains"),
    ("只有表、视图和触发器可以读取只读定义", "Only tables, views and triggers have a read-only definition"),
    // 活动外层事务里的重建（ADR-4）：内部事务换成保存点边界。
    (
        "SQLite 不能在事务里执行 VACUUM：先在当前标签提交或回滚事务，不会另开会话绕过。",
        "SQLite can't run VACUUM inside a transaction: commit or roll back the transaction first; no other session is opened to bypass it.",
    ),
    (
        "活动事务里 PRAGMA foreign_keys 不生效：先提交或回滚事务再改外键开关，不会另开会话绕过。",
        "PRAGMA foreign_keys has no effect inside an active transaction: commit or roll back first, then change the switch; no other session is opened to bypass it.",
    ),
    (
        "活动事务里不能改变 journal_mode：先提交或回滚事务，不会另开会话绕过。",
        "journal_mode can't be changed inside an active transaction: commit or roll back first; no other session is opened to bypass it.",
    ),
    ("开始重建保存点", "Begin the rebuild savepoint"),
    ("释放重建保存点（外层事务仍未提交）", "Release the rebuild savepoint (the outer transaction is still not committed)"),
    (
        "活动事务里不能在事务内关闭外键检查：重建按当前外键设置执行；被其他表引用时数据库可能拒绝删除旧表。失败只回滚到重建保存点，不会撤销此前已经应用的修改。",
        "Foreign key checks can't be turned off inside an active transaction: the rebuild runs with the current setting and the database may refuse to drop a table that other tables reference. A failure only rolls back to the rebuild savepoint and doesn't undo changes already applied.",
    ),
    (
        "活动事务里的 SQLite 重建：在内部保存点里建新表、复制数据、删除旧表并改回原名，然后做 foreign_key_check（外键完整性检查）。不能关闭外键检查；失败只回滚到这个保存点，不会撤销此前已经应用的修改。重建结果随当前事务一起提交或回滚。",
        "SQLite rebuild inside an active transaction: a savepoint creates the new table, copies the data, drops the old table and renames the new one back, followed by foreign_key_check (a foreign key integrity check). Foreign key checks can't be turned off; a failure only rolls back to this savepoint and doesn't undo changes already applied. The rebuild is committed or rolled back with the current transaction.",
    ),
    (
        "已回滚到重建保存点：这条语句没有保留，此前已经应用的修改仍在当前事务里。",
        "Rolled back to the rebuild savepoint: this statement was not kept, and changes already applied remain in the current transaction.",
    ),
    (
        "回滚到重建保存点失败，结果未知：{}",
        "Rolling back to the rebuild savepoint failed, so the result is unknown: {0}",
    ),
];

/// 重建定义“未包含”清单里这个引擎专属的项。
pub(in crate::adapters) const MISSING_ITEMS_EN: &[(&str, &str)] = &[];
