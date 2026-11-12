//! 结构变更计划里给人看的文字（风险、阻断原因、步骤名）的英文版。
//!
//! 计划由执行层按中文生成，确认执行时还要和同一份计划逐条比对 SQL，所以不在生成端分语言；
//! 英文界面在显示时按这里的句型表换成英文。句型里的 `{}` 是对象名、行数等可变部分，
//! 英文句子用 `{0}`、`{1}` 按位置取回（可变部分本身是已知句型时也一起换）。
//! 覆盖范围：`ddl/` 与 `application/ddl.rs` 非测试源码里的每一句中文（计划风险/阻断/步骤、读取层与执行层错误、
//! 执行结果），以及确认层、对象定义查看器（`catalog/definition.rs` 与 `catalog/read.rs` 的定义读取函数）和结构页
//! 路径上的中文，由源码扫描测试 `every_chinese_literal_on_ddl_paths_has_an_english_form` 守住；只用于匹配的片段与
//! 写进生成 SQL 注释的文字列在测试白名单里并写明原因。查不到句型的原样返回（数据库原文不翻译）。

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
        "当前连接 foreign_keys = {p}。不会为了让这次操作看起来成功而静默打开或关闭它。",
        "This connection has foreign_keys = {0}. It is never switched on or off silently to make this operation look successful.",
    ),
    ("Redis 没有结构编辑。", "Redis has no structure editing."),
    ("MongoDB 没有结构编辑。", "MongoDB has no structure editing."),
    ("当前连接打开的是 {}，没有另开连接修改 {}", "This connection has {0} open; no other connection was opened to change {1}"),
    ("需要数据库名，没有另开连接。", "A database name is required; no other connection was opened."),
    ("PostgreSQL 需要指定 schema（表所在的分组名），没有另开连接。", "PostgreSQL needs a schema (the group the table is in); no other connection was opened."),
    ("SQL Server 需要指定 schema（表所在的分组名），没有另开连接。", "SQL Server needs a schema (the group the table is in); no other connection was opened."),
    ("被这些外键引用：{l}。{p}", "Referenced by these foreign keys: {0}. {1}"),
    // 删除表
    ("将永久删除 {} 的结构和{p}。这不是备份。", "This permanently deletes the structure of {0} and {1}. This is not a backup."),
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
    ("这些视图可能引用它：{l}。数据库可能拒绝删除，或让它们失效。", "These views may reference it: {0}. The database may refuse the drop or leave them invalid."),
    // 清空表
    (
        "将永久删除 {} 里的{p}数据，只保留表结构。这不是备份，也不能撤销。",
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
    ("这些视图或触发器引用了它：{l}。数据库不一定会同步改写它们。", "These views or triggers reference it: {0}. The database won't necessarily update them."),
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
    ("{} 已经是 {} 的值，没有生成语句。", "{0} is already a value of {1}, so no statement was generated."),
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
    // 结构读取（introspect）：读不到、找不到对象与读取结果。
    ("找不到表或视图 {}", "Table or view {0} was not found"),
    ("表名无效。", "The table name is not valid."),
    ("没有目标表", "There is no target table"),
    ("这个动作需要指定表，不是数据库级动作。", "This action needs a table; it is not a database-level action."),
    ("目标 {} 不是基础表。", "The target {0} is not a base table."),
    ("读不到序列 {}：{}", "Sequence {0} could not be read: {1}"),
    ("找不到序列 {}。", "Sequence {0} was not found."),
    ("读不到类型 {}：{}", "Type {0} could not be read: {1}"),
    ("找不到类型 {}。", "Type {0} was not found."),
    ("{} 不是枚举类型，枚举值只能加在枚举上。", "{0} is not an enum type; enum values can only be added to an enum."),
    ("读不到枚举 {} 的取值：{}", "The values of enum {0} could not be read: {1}"),
    ("{} 不是域（domain）。", "{0} is not a domain."),
    ("读不到域 {} 的定义：{}", "The definition of domain {0} could not be read: {1}"),
    ("找不到域 {}。", "Domain {0} was not found."),
    ("读不到事件 {}：{}", "Event {0} could not be read: {1}"),
    ("找不到事件 {}。", "Event {0} was not found."),
    ("没有读到 NULL 计数", "The NULL count could not be read"),
    ("没有读到行数", "The row count could not be read"),
    ("没有读到视图定义", "The view definition could not be read"),
    ("不是整数 {}", "Not an integer: {0}"),
    ("元数据是空的", "The metadata is empty"),
    ("读不到第 {} 列", "Column {0} could not be read"),
    ("由主键约束维护", "Maintained by the primary key constraint"),
    ("由唯一约束维护", "Maintained by a unique constraint"),
    ("由唯一约束 {} 维护", "Maintained by unique constraint {0}"),
    ("外键 {} 依赖此索引", "Foreign key {0} depends on this index"),
    ("VACUUM 已完成：数据库现在有 {} 页，其中空闲 {} 页。", "VACUUM finished: the database now has {0} pages, {1} of them free."),
    ("VACUUM 已完成。", "VACUUM finished."),
    ("REINDEX 已完成：索引已按当前的表结构重建。", "REINDEX finished: the indexes were rebuilt for the current table structure."),
    // 结构编辑：建表、字段、索引、外键。
    ("至少需要一个字段。", "At least one column is needed."),
    ("表 {} 已经存在。不会覆盖已有表或数据。", "Table {0} already exists. Existing tables and data are not overwritten."),
    (
        "创建一张新表。同名对象已存在时数据库会报错，不会覆盖已有表或数据。",
        "Creates a new table. If an object with the same name exists, the database reports an error; existing tables and data are not overwritten.",
    ),
    (
        "没有主键：之后可以新增行，但不能在表格里修改或删除已有行。",
        "No primary key: rows can be added later, but existing rows can't be edited or deleted in the grid.",
    ),
    ("创建 {}", "Create {0}"),
    ("没有字段变更。", "There are no column changes."),
    ("索引名称无效。", "The index name is not valid."),
    ("索引 {} 已经存在，不会覆盖。", "Index {0} already exists; it won't be overwritten."),
    ("索引至少需要一列。", "An index needs at least one column."),
    ("列 {} 不在表 {} 中。", "Column {0} is not in table {1}."),
    ("列 {} 在索引里重复。", "Column {0} appears twice in the index."),
    ("按 {} 的顺序创建唯一索引。", "Creates a unique index in the order {0}."),
    ("按 {} 的顺序创建普通索引。", "Creates a regular index in the order {0}."),
    ("已有重复值时创建会失败。不会删除或修改任何数据。", "Creation fails if duplicate values exist. No data is deleted or changed."),
    ("创建索引 {}", "Create index {0}"),
    ("找不到索引 {}", "Index {0} was not found"),
    ("索引 {} 由约束维护，不能当作普通索引单独删除。", "Index {0} is maintained by a constraint and can't be dropped on its own like a regular index."),
    ("索引 {} 不能单独删除：{p}", "Index {0} can't be dropped on its own: {1}"),
    ("只删除这个索引，不删除表或记录。", "Only this index is dropped; the table and its rows stay."),
    ("删除索引 {}", "Drop index {0}"),
    (
        "表 {} 使用 MyISAM，不支持外键。不会通过关闭检查来假装创建成功。",
        "Table {0} uses MyISAM, which doesn't support foreign keys. Checks are not turned off to pretend the creation succeeded.",
    ),
    (
        "数据库会检查已有数据。存在无法匹配的值时创建失败。不会关闭约束检查，也不会清理数据。",
        "The database checks the existing data. Creation fails if values don't match. Constraint checks are not turned off and no data is cleaned up.",
    ),
    ("CASCADE 会在父表变更时连带修改或删除本表中引用它的行。", "CASCADE changes or deletes the rows in this table that reference the parent table when the parent changes."),
    ("SQLite 不能直接 ALTER TABLE 添加外键，需要按官方步骤重建表。", "SQLite can't add a foreign key with ALTER TABLE; the table has to be rebuilt by the official steps."),
    ("创建外键 {}", "Create foreign key {0}"),
    ("找不到外键 {}", "Foreign key {0} was not found"),
    ("只删除这条外键，不删除父表、子表或记录。", "Only this foreign key is dropped; the parent table, child table and rows stay."),
    ("SQLite 需要重建表才能移除外键。", "SQLite has to rebuild the table to remove a foreign key."),
    ("删除外键 {}", "Drop foreign key {0}"),
    ("外键名称无效。", "The foreign key name is not valid."),
    ("外键 {} 已经存在，不会覆盖。", "Foreign key {0} already exists; it won't be overwritten."),
    ("外键两侧的列数量必须一致。", "Both sides of the foreign key need the same number of columns."),
    ("ON DELETE {} 不是 {} 支持的引用动作。", "ON DELETE {0} is not a referential action {1} supports."),
    ("ON UPDATE {} 不是 {} 支持的引用动作。", "ON UPDATE {0} is not a referential action {1} supports."),
    ("本表没有列 {}。", "This table has no column {0}."),
    ("本表列 {} 在外键里重复。", "Column {0} of this table appears twice in the foreign key."),
    ("目标表和预检查读到的表不一致。", "The target table doesn't match the table read by the pre-check."),
    ("目标表没有列 {}。", "The target table has no column {0}."),
    ("类型不匹配：{}（{}）和 {}（{}）。", "Type mismatch: {0} ({1}) and {2} ({3})."),
    ("目标列必须构成目标表的主键或唯一约束。", "The target columns must form the primary key or a unique constraint of the target table."),
    ("没有读到目标表，不能确认外键是否有效。", "The target table could not be read, so the foreign key can't be confirmed as valid."),
    ("类型 {} 无效。", "Type {0} is not valid."),
    ("默认值 {} 无效。", "Default value {0} is not valid."),
    ("第 {} 个字段的名称无效。", "The name of column {0} is not valid."),
    ("字段名 {} 重复。", "Column name {0} is used twice."),
    ("字段 {} 的类型 {} 不能按 {} 规则生成。", "The type {1} of column {0} can't be generated by the {2} rules."),
    ("字段 {} 的默认值不是允许的字面量或当前时间函数。", "The default of column {0} is not an allowed literal or current-time function."),
    ("字段 {} 只有主键才能自增。", "Column {0} can only auto-increment as the primary key."),
    // 字段变更
    ("字段 {} 没有出现在变更里，已停止以免漏改。", "Column {0} is missing from the changes; stopped so nothing is changed by mistake."),
    ("不能删除主键列 {}。", "Primary key column {0} can't be dropped."),
    ("不能删除生成列 {}。", "Generated column {0} can't be dropped."),
    ("外键引用列 {}，删除它不能保留该约束。", "A foreign key uses column {0}; dropping it can't keep that constraint."),
    ("其他表的外键引用列 {}，删除它不能保留该约束。", "A foreign key in another table references column {0}; dropping it can't keep that constraint."),
    ("不能给已有表追加主键列 {}。", "Primary key column {0} can't be added to an existing table."),
    ("SQLite 新增非空字段 {} 必须有非 NULL 默认值。", "In SQLite, the new NOT NULL column {0} needs a non-NULL default."),
    ("新增非空字段 {} 没有默认值，已有行无法满足 NOT NULL。", "The new NOT NULL column {0} has no default, so existing rows can't satisfy NOT NULL."),
    (
        "没能确认表 {} 是否已有数据，不能新增没有默认值的非空字段 {}。",
        "It couldn't be confirmed whether table {0} has data, so the NOT NULL column {1} without a default can't be added.",
    ),
    ("生成列 {} 不能这样修改。", "Generated column {0} can't be changed this way."),
    ("不能在这里修改主键列 {} 的类型。", "The type of primary key column {0} can't be changed here."),
    ("不能在这里修改主键列 {} 的空值约束。", "The NULL constraint of primary key column {0} can't be changed here."),
    ("不能把 {} 改成主键。", "{0} can't be made the primary key."),
    (
        "列 {} 有 {} 个 NULL，不能设为 NOT NULL。请先改数据，或保留允许 NULL。",
        "Column {0} has {1} NULLs, so it can't be set to NOT NULL. Change the data first, or keep allowing NULL.",
    ),
    ("没能完成列 {} 的 NULL 预检查，没有生成变更。", "The NULL pre-check of column {0} couldn't finish, so no change was generated."),
    ("不能在这里改变主键组成。", "The primary key columns can't be changed here."),
    ("删除字段 {} 会永久丢掉该列的数据。", "Dropping column {0} permanently loses the data in that column."),
    (
        "字段 {} 的类型由 {} 改为 {}。无法转换的值会使执行失败；RillSQL 不会自动截断或清空数据。",
        "The type of column {0} changes from {1} to {2}. Values that can't be converted make the run fail; RillSQL never truncates or clears data on its own.",
    ),
    ("重命名 {} → {}。仍使用旧列名的查询或应用需要同步修改。", "Rename {0} → {1}. Queries and applications that still use the old column name need to be updated."),
    ("重命名 {} → {}", "Rename {0} → {1}"),
    ("新增字段 {}", "Add column {0}"),
    ("删除字段 {}", "Drop column {0}"),
    ("修改类型 {}", "Change type of {0}"),
    ("修改默认值 {}", "Change default of {0}"),
    ("删除默认值 {}", "Drop default of {0}"),
    ("删除原默认值 {}", "Drop the old default of {0}"),
    ("修改字段 {}", "Change column {0}"),
    ("去掉 NOT NULL {}", "Drop NOT NULL on {0}"),
    ("设置 NOT NULL {}", "Set NOT NULL on {0}"),
    ("允许 NULL {}", "Allow NULL on {0}"),
    // SQLite 重建表
    (
        "需要重建表。SQLite 不能直接完成这项修改，将按官方 12 步新建表、复制数据并替换原表。",
        "The table needs a rebuild. SQLite can't make this change directly, so the official 12 steps create a new table, copy the data and replace the original.",
    ),
    ("索引 {} 引用了被删除或无法原样保留的列，重建时不会保留它。", "Index {0} uses a column that is dropped or can't be kept as is, so the rebuild won't keep it."),
    ("没有可以可靠复制的原有列，因此阻止重建。", "No original column can be copied reliably, so the rebuild is blocked."),
    ("复制 {} 行", "Copy {0} rows"),
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
    (
        "WITHOUT ROWID 表必须保留主键，重建不能可靠保留行标识，因此阻止执行。",
        "A WITHOUT ROWID table must keep its primary key and a rebuild can't keep the row identity reliably, so it is blocked.",
    ),
    ("触发器 {} 引用了将改变的列，无法可靠改写，因此阻止重建。", "Trigger {0} uses columns that will change and can't be rewritten reliably, so the rebuild is blocked."),
    ("读不到触发器 {} 的定义，不能可靠重建。", "The definition of trigger {0} couldn't be read, so it can't be rebuilt reliably."),
    ("视图 {} 引用了将改变的结构，无法可靠改写，因此阻止重建。", "View {0} uses structure that will change and can't be rewritten reliably, so the rebuild is blocked."),
    ("外键 {} 引用了将改变的列，重建不能可靠保留该约束。", "Foreign key {0} uses columns that will change, so the rebuild can't keep that constraint reliably."),
    ("索引 {} 含表达式或部分条件，列被重命名后无法可靠改写。", "Index {0} has an expression or partial condition and can't be rewritten reliably after a column rename."),
    ("读不到索引 {} 的原始语句，不能可靠保留。", "The original statement of index {0} couldn't be read, so it can't be kept reliably."),
    ("读不到索引 {} 的原始语句。", "The original statement of index {0} couldn't be read."),
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
    // 枚举与域（PostgreSQL）
    ("枚举值只能追加，不能删除或调整顺序；已有的行不会被改写。", "Enum values can only be appended, not removed or reordered; existing rows are not rewritten."),
    ("只有 PostgreSQL 有枚举类型，这里不生成语句。", "Only PostgreSQL has enum types, so no statement is generated here."),
    ("枚举名无效。", "The enum name is not valid."),
    ("枚举值不能为空。", "The enum value can't be empty."),
    ("枚举值里不能有控制字符。", "The enum value can't contain control characters."),
    ("PostgreSQL 的枚举值最长 63 字节。", "PostgreSQL enum values are at most 63 bytes long."),
    (
        "没有读到枚举 {} 的现有取值（可能不是枚举或权限不足），没有生成语句。",
        "The existing values of enum {0} could not be read (it may not be an enum, or a privilege is missing), so no statement was generated.",
    ),
    ("找不到枚举值 {}，没有生成语句。", "Enum value {0} was not found, so no statement was generated."),
    ("不能把新值放在它自己前面。", "The new value can't be placed before itself."),
    (
        "新值要在这条语句执行完之后才能被使用；这里单条执行，不把追加值和使用它的语句放进同一个事务。",
        "The new value can only be used after this statement finishes; it runs on its own and is not put in one transaction with statements that use it.",
    ),
    ("给 {} 追加值 {}", "Add value {1} to {0}"),
    ("域的默认值只影响之后插入的行，不改已有数据。", "A domain default only affects rows inserted later; existing data is not changed."),
    ("只有 PostgreSQL 有域（domain）对象，这里不生成语句。", "Only PostgreSQL has domain objects, so no statement is generated here."),
    ("域名无效。", "The domain name is not valid."),
    ("没有读到域 {} 的定义，没有生成语句。", "The definition of domain {0} could not be read, so no statement was generated."),
    ("删除域默认值", "Drop the domain default"),
    ("默认值只接受字面量、TRUE/FALSE、now() 或 CURRENT_TIMESTAMP。", "The default only accepts a literal, TRUE/FALSE, now() or CURRENT_TIMESTAMP."),
    ("修改域默认值", "Change the domain default"),
    ("修改域 NOT NULL", "Change the domain NOT NULL"),
    (
        "SET NOT NULL 会先检查已有数据，存在空值时数据库会报错；这里不先改写数据。",
        "SET NOT NULL checks the existing data first and the database reports an error if there are nulls; the data is not rewritten beforehand.",
    ),
    ("域 {} 的默认值与 NOT NULL 没有变化。", "The default and NOT NULL of domain {0} have not changed."),
    // 视图定义替换
    ("视图 {} 已失效，没有发送。", "View {0} is no longer valid, so nothing was sent."),
    ("目标视图和已读取的结构不一致，没有生成语句。", "The target view doesn't match the structure that was read, so no statement was generated."),
    ("只有视图可以替换定义。函数、存储过程和触发器不能从这里保存。", "Only views can have their definition replaced. Functions, stored procedures and triggers can't be saved from here."),
    ("没有读到可编辑的视图定义：{p}", "No editable view definition could be read: {0}"),
    ("定义只剩空白和注释，没有发送。", "The definition is only whitespace and comments, so nothing was sent."),
    ("无法在不丢弃物化数据的情况下替换这个物化视图，因此不发送。", "This materialized view can't be replaced without discarding its materialized data, so nothing is sent."),
    ("视图被 {l} 依赖，无法在不丢弃这些依赖的情况下替换，因此不发送。", "The view is used by {0} and can't be replaced without discarding them, so nothing is sent."),
    ("定义没有改动，没有发送。", "The definition hasn't changed, so nothing was sent."),
    ("定义包含多条语句，无法安全替换，没有发送。", "The definition has more than one statement and can't be replaced safely, so nothing was sent."),
    ("这条语句创建的不是当前视图，没有发送。", "This statement doesn't create the current view, so nothing was sent."),
    ("这段定义不能安全地替换当前视图，没有发送。", "This definition can't safely replace the current view, so nothing was sent."),
    ("将按下面的顺序替换视图定义。取消不会发送任何语句。", "The view definition is replaced in the order below. Canceling sends no statement."),
    ("创建视图 {}", "Create view {0}"),
    ("替换视图 {}", "Replace view {0}"),
    // 执行结果与确认层错误（执行层、应用层与界面生成的话；数据库原文不在这里）。
    (
        "预览已过期：数据库里的结构或生成的 SQL 和确认时看到的不一致，没有执行。",
        "The preview is out of date: the structure in the database or the generated SQL no longer matches what you confirmed, so nothing was run.",
    ),
    ("前面的语句没有成功，这条没有执行。", "An earlier statement didn't succeed, so this one was not run."),
    ("这条连接还没有打开，没有另开新的连接。", "This connection isn't open yet; no new connection was opened."),
    ("结构变更需要先确认预览中的 SQL，未确认不会执行。", "Structure changes need the previewed SQL to be confirmed first; nothing runs without confirmation."),
    ("这个连接没有连着可查询的数据库。", "This connection isn't connected to a database that can be queried."),
    ("会话类型和目标数据库不一致，没有另开连接。", "The session type doesn't match the target database; no other connection was opened."),
    ("foreign_key_check 发现 {} 处违规，不会把这次重建当成成功。", "foreign_key_check found {0} violations, so this rebuild is not treated as a success."),
    ("不支持的数据库 {}", "Unsupported database {0}"),
    ("没有执行任何语句", "No statement was run"),
    ("生成计划失败", "Generating the plan failed"),
    ("执行失败", "Execution failed"),
    // 对象定义查看器（catalog/definition.rs 与 catalog/read.rs 的定义读取）。查看器显示前先去掉“读取定义失败：”前缀，
    // 这两条留给没有去前缀的地方。
    ("读取定义失败：{p}", "Could not read the definition: {0}"),
    ("读取定义失败", "Could not read the definition"),
    ("{} 的{}没有可读的定义", "There is no readable definition for a {1} on {0}"),
    ("{} 不是基础表", "{0} is not a base table"),
    ("缺少对象名", "The object name is missing"),
    ("这个连接没有连着可查询的数据库", "This connection isn't connected to a database that can be queried"),
    ("Redis 没有对象定义", "Redis has no object definitions"),
    ("MongoDB 集合没有 SQL 定义；文档在集合浏览器查看", "MongoDB collections have no SQL definition; view the documents in the collection browser"),
    ("SQLite 没有函数或存储过程", "SQLite has no functions or stored procedures"),
    ("SQLite 没有序列、枚举或域", "SQLite has no sequences, enums or domains"),
    ("只有表、视图和触发器可以读取只读定义", "Only tables, views and triggers have a read-only definition"),
    ("没有读到定义", "No definition was returned"),
    ("MariaDB 没有独立的枚举或域", "MariaDB has no separate enums or domains"),
    ("MySQL 没有独立的序列、枚举或域", "MySQL has no separate sequences, enums or domains"),
    (
        "只有表、视图、函数、存储过程、触发器和事件可以读取只读定义",
        "Only tables, views, functions, stored procedures, triggers and events have a read-only definition",
    ),
    ("需要数据库名", "A database name is required"),
    ("PostgreSQL 需要 schema", "PostgreSQL needs a schema"),
    ("有多个同名函数，需要用参数类型区分", "Several functions have this name; their argument types are needed to tell them apart"),
    ("有多个同名触发器，需要所属的表才能区分", "Several triggers have this name; their table is needed to tell them apart"),
    (
        "只有视图、函数、存储过程、触发器、序列、枚举和域可以读取只读定义",
        "Only views, functions, stored procedures, triggers, sequences, enums and domains have a read-only definition",
    ),
    ("SQL Server 需要 schema", "SQL Server needs a schema"),
    ("有多个同名对象，需要更具体的名称", "Several objects have this name; a more specific name is needed"),
    ("没有读到序列起点", "The sequence start value was not returned"),
    ("没有读到序列步长", "The sequence increment was not returned"),
    ("没有读到序列下限", "The sequence minimum was not returned"),
    ("没有读到序列上限", "The sequence maximum was not returned"),
    ("SQL Server 没有独立的枚举或域", "SQL Server has no separate enums or domains"),
    ("只有函数、存储过程、触发器、视图和序列可以读取只读定义", "Only functions, stored procedures, triggers, views and sequences have a read-only definition"),
    ("读取库表失败：{p}", "Could not read the catalog: {0}"),
    ("读取{p}失败：{p}", "Could not read {0}: {1}"),
    ("元数据列缺失", "A metadata column is missing"),
    ("元数据列是空值", "A metadata column is null"),
];

/// 重建的建表定义“未包含”清单里的项（`ddl::rebuilt_definition` 返回的 `missing`）。查看器横幅逐项显示；
/// 同一份清单写进生成 SQL 开头的注释时保持中文。只按整项匹配，不参与整句翻译。
const MISSING_ITEMS: &[(&str, &str)] = &[
    ("CHECK 约束", "CHECK constraints"),
    ("分区", "partitioning"),
    ("存储参数与表空间", "storage parameters and tablespace"),
    ("权限", "privileges"),
    ("触发器", "triggers"),
    ("计算列表达式", "computed column expressions"),
    ("文件组与压缩选项", "filegroup and compression options"),
    ("主键的 CLUSTERED / NONCLUSTERED 选项（按 SQL Server 默认处理）", "the primary key's CLUSTERED / NONCLUSTERED option (SQL Server's default is assumed)"),
    ("索引选项（FILLFACTOR、IGNORE_DUP_KEY 等）", "index options (FILLFACTOR, IGNORE_DUP_KEY and so on)"),
    ("XML 与空间索引", "XML and spatial indexes"),
    (
        "标识列的 ALWAYS / BY DEFAULT 模式与序列参数（统一写成 BY DEFAULT）",
        "the identity column's ALWAYS / BY DEFAULT mode and sequence options (written as BY DEFAULT)",
    ),
    ("IDENTITY 的起点与步长（统一写成 1, 1）", "the IDENTITY seed and increment (written as 1, 1)"),
    ("索引（读取失败：{p}）", "indexes (could not be read: {0})"),
    ("外键（读取失败：{p}）", "foreign keys (could not be read: {0})"),
    ("注释（读取失败：{p}）", "comments (could not be read: {0})"),
];

/// 只出现在句型 `{p}` 位置里的短语（行数、“全部”、“未知”）。不参与整句匹配，
/// 所以名字恰好是“未知”“全部”的对象不会被换成英文词。
const PHRASES: &[(&str, &str)] = &[
    ("约 {} 行", "about {0} rows"),
    ("现有数据", "its existing data"),
    ("全部", "all"),
    ("未知", "unknown"),
    ("事件与分区", "events and partitions"),
    ("附加库", "attached databases"),
    ("没有返回值", "no value was returned"),
    ("扩展", "extensions"),
    ("序列", "sequences"),
    ("事件", "events"),
    ("分区", "partitions"),
    ("枚举", "enums"),
    ("域", "domains"),
];

/// 是否含中日韩文字或全角标点。
pub fn has_cjk(text: &str) -> bool {
    text.chars().any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch) || ('\u{3000}'..='\u{303f}').contains(&ch) || ('\u{ff00}'..='\u{ffef}').contains(&ch))
}

/// 句型里可变部分的种类：`{}` 名字/数值，原样保留；`{l}` 用顿号连起来的名字列表，只把顿号换成逗号；
/// `{p}` 已知短语或句子（行数、执行规则等），按短语表和句型表换成英文。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Name,
    List,
    Phrase,
}

/// 把句型拆成字面段和可变部分；字面段比可变部分多一段。
fn split_pattern(pattern: &str) -> (Vec<&str>, Vec<Slot>) {
    let mut parts = Vec::new();
    let mut slots = Vec::new();
    let mut rest = pattern;
    loop {
        let next = [("{}", Slot::Name), ("{l}", Slot::List), ("{p}", Slot::Phrase)]
            .into_iter()
            .filter_map(|(marker, slot)| rest.find(marker).map(|at| (at, marker.len(), slot)))
            .min_by_key(|(at, _, _)| *at);
        match next {
            Some((at, len, slot)) => {
                parts.push(&rest[..at]);
                slots.push(slot);
                rest = &rest[at + len..];
            }
            None => {
                parts.push(rest);
                return (parts, slots);
            }
        }
    }
}

/// 按句型把 `text` 拆成可变部分（可以为空，例如还没填的新名字）；不匹配返回 None。
/// 中间的可变部分取最短匹配，最后一段吃到结尾。
fn captures(pattern: &str, text: &str) -> Option<Vec<(Slot, String)>> {
    let (parts, slots) = split_pattern(pattern);
    let mut rest = text.strip_prefix(parts[0])?;
    if parts.len() == 1 {
        return rest.is_empty().then(Vec::new);
    }
    let mut values = Vec::new();
    for (index, literal) in parts.iter().enumerate().skip(1) {
        if index == parts.len() - 1 {
            values.push((slots[index - 1], rest.strip_suffix(literal)?.to_owned()));
        } else {
            if literal.is_empty() {
                return None;
            }
            let at = rest.find(literal)?;
            values.push((slots[index - 1], rest[..at].to_owned()));
            rest = &rest[at + literal.len()..];
        }
    }
    Some(values)
}

/// 可变部分按种类换成英文：名字原样，列表只换分隔符，短语查短语表和句型表（查不到原样）。
fn value_in_english(slot: Slot, value: &str) -> String {
    match slot {
        Slot::Name => value.to_owned(),
        Slot::List => value.replace('、', ", "),
        Slot::Phrase if !has_cjk(value) => value.to_owned(),
        Slot::Phrase => translate_with(PHRASES, value).or_else(|| translate(value)).unwrap_or_else(|| value.to_owned()),
    }
}

fn translate_with(table: &[(&str, &str)], text: &str) -> Option<String> {
    table.iter().find_map(|(zh, en)| {
        let values = captures(zh, text)?;
        let mut out = (*en).to_owned();
        for (index, (slot, value)) in values.iter().enumerate() {
            out = out.replace(&format!("{{{index}}}"), &value_in_english(*slot, value));
        }
        Some(out)
    })
}

fn translate(text: &str) -> Option<String> {
    translate_with(PATTERNS, text)
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

/// 按界面语言给出重建定义“未包含”清单里的一项；不是已知项时按整句再查一次，仍查不到原样返回。
pub fn localize_missing_item(item: &str, locale: crate::i18n::Locale) -> String {
    match locale {
        crate::i18n::Locale::En if has_cjk(item) => translate_with(MISSING_ITEMS, item).unwrap_or_else(|| english(item)),
        _ => item.to_owned(),
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

    /// 对象名位置不做整句翻译、不换顿号：名为“未知”“全部”“a、b”的对象在英文句子里原样出现；只有已知短语位置换成英文。
    #[test]
    fn object_name_slots_keep_the_name_verbatim() {
        assert_eq!(english("删除视图 未知"), "Drop view 未知");
        assert_eq!(english("删除 a、b"), "Drop a、b");
        assert_eq!(english("清空 全部"), "Empty 全部");
        assert_eq!(
            english("将把 未知 改名为 a、b。引用旧名的查询、视图、存储过程和应用代码可能失败。"),
            "This renames 未知 to a、b. Queries, views, stored procedures and application code that use the old name may fail."
        );
        // 短语位置仍然换成英文，名字位置不动。
        assert_eq!(
            english("将永久删除 全部 里的全部数据，只保留表结构。这不是备份，也不能撤销。"),
            "This permanently deletes all of the data in 全部 and keeps only the table structure. This is not a backup and can't be undone."
        );
        assert_eq!(
            english("将永久删除 现有数据 的结构和现有数据。这不是备份。"),
            "This permanently deletes the structure of 现有数据 and its existing data. This is not a backup."
        );
        assert_eq!(
            english("当前连接 foreign_keys = 未知。不会为了让这次操作看起来成功而静默打开或关闭它。"),
            "This connection has foreign_keys = unknown. It is never switched on or off silently to make this operation look successful."
        );
        // 短语不参与整句匹配。
        assert_eq!(english("未知"), "未知");
        // 经过计划生成：视图名“未知”、表名“a、b”在英文风险和步骤里原样。
        let view = snapshot(Engine::Sqlite, "view", "未知");
        let dropped = plan(&view, &Action::DropView { view: "未知".into() });
        assert_eq!(english(&dropped.steps[0].label), "Drop view 未知");
        let risk = english(&dropped.risks[0].text);
        assert!(risk.starts_with("This permanently deletes the definition of view ") && risk.contains("未知") && !risk.contains("unknown"), "{risk}");
        let table = snapshot(Engine::Sqlite, "table", "a、b");
        let truncated = plan(&table, &Action::TruncateTable { table: "a、b".into() });
        assert_eq!(english(&truncated.steps[0].label), "Empty a、b");
        assert!(english(&truncated.risks[0].text).contains("a、b"), "{}", english(&truncated.risks[0].text));
    }

    /// 读出源码里非测试部分的字符串字面量（跳过注释与字符字面量），只留含中文的。
    fn chinese_literals(source: &str) -> Vec<String> {
        let bytes = source.as_bytes();
        let mut out = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            match bytes[at] {
                b'/' if bytes.get(at + 1) == Some(&b'/') => {
                    at = source[at..].find('\n').map_or(bytes.len(), |end| at + end);
                }
                b'/' if bytes.get(at + 1) == Some(&b'*') => {
                    at = source[at + 2..].find("*/").map_or(bytes.len(), |end| at + 2 + end + 2);
                }
                b'"' => {
                    let start = at + 1;
                    at = start;
                    while bytes[at] != b'"' {
                        at += if bytes[at] == b'\\' { 2 } else { 1 };
                    }
                    let literal = &source[start..at];
                    if has_cjk(literal) {
                        out.push(literal.to_owned());
                    }
                    at += 1;
                }
                b'\'' if bytes.get(at + 1) == Some(&b'\\') => {
                    at = source[at + 2..].find('\'').map_or(bytes.len(), |end| at + 2 + end + 1);
                }
                b'\'' => {
                    // 字符字面量 'x'（含多字节字符）；否则是生命周期标记。
                    let width = source[at + 1..].chars().next().map_or(1, char::len_utf8);
                    at += if bytes.get(at + 1 + width) == Some(&b'\'') { width + 2 } else { 1 };
                }
                _ => at += 1,
            }
        }
        out
    }

    /// 把 format 的占位（`{}`、`{name}`、`{:?}`）换成不同的英文样例值；`{{`/`}}` 还原成花括号。
    fn fill_placeholders(literal: &str) -> String {
        let mut out = String::new();
        let mut rest = literal;
        let mut index = 0;
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            if rest[open..].starts_with("{{") {
                out.push('{');
                rest = &rest[open + 2..];
                continue;
            }
            let close = rest[open..].find('}').map_or(rest.len(), |end| open + end);
            out.push_str(&format!("v{index}"));
            index += 1;
            rest = rest.get(close + 1..).unwrap_or_default();
        }
        out.push_str(rest);
        out.replace("}}", "}")
    }

    /// 不是给人看的整句：只用来匹配/拼接的片段，或写进生成 SQL 注释里的文字。每条写明原因。
    const NOT_MESSAGES: &[(&str, &str)] = &[
        ("找不到表或视图", "exec.rs 用 starts_with 识别读取层“找不到表或视图 {}”错误的前缀，本身不显示"),
        ("需要", "plan.rs 用 contains 识别 sql.rs 返回的限定名错误，本身不显示"),
        ("没有另开", "plan.rs 用 contains 识别 sql.rs 返回的限定名错误，本身不显示"),
        ("主键", "introspect.rs 用 contains 在索引原因里认出主键索引，本身不显示"),
        ("、", "名字列表的分隔符，英文句型的 {l} 位置把它换成逗号"),
        (
            "-- 由元数据重建，不是数据库保存的原文。未包含：{}。\\nCREATE TABLE {qualified} (\\n  {}\\n);",
            "重建的建表语句开头的 SQL 注释，属于生成的 SQL 文本（查看器编辑区与复制内容），不按界面语言改写；查看器横幅另按 MISSING_ITEMS 逐项英文化",
        ),
        ("-- 索引 {name} 的类型是 {}，没有从元数据重建。", "重建的建表语句里的 SQL 注释，属于生成的 SQL 文本（查看器编辑区与复制内容），不按界面语言改写"),
    ];

    /// 取源码里一个函数（`fn name(` 到与它同一缩进的收尾 `}`）的文本，只扫描确认层/查看器/结构页路径上的函数。
    /// 工作区可能是 CRLF（Windows 检出），先统一成 LF 再切。
    fn function_source(source: &str, name: &str) -> String {
        let source = source.replace("\r\n", "\n");
        let at = source.find(&format!("fn {name}(")).unwrap_or_else(|| panic!("源码里没有函数 {name}"));
        let line = source[..at].rfind('\n').map_or(0, |end| end + 1);
        let indent: String = source[line..at].chars().take_while(|ch| *ch == ' ').collect();
        let close = format!("\n{indent}}}\n");
        let end = source[at..].find(&close).unwrap_or_else(|| panic!("函数 {name} 没有找到收尾")) + at + close.len();
        source[line..end].to_owned()
    }

    fn non_test(source: &str) -> String {
        let source = source.replace("\r\n", "\n");
        source.split("\nmod tests").next().unwrap_or_default().to_owned()
    }

    /// 一句中文在界面上的英文：整句查句型表、只出现在短语位置的片段查短语表、重建定义“未包含”项查清单表、
    /// 定义读取错误按查看器的显示方式（去掉“读取定义失败：”前缀）再查。任一种得到不含中文的结果即算有英文。
    fn has_english_form(sample: &str) -> Result<(), String> {
        let sentence = english(sample);
        if !has_cjk(&sentence) {
            return Ok(());
        }
        let candidates = [
            translate_with(PHRASES, sample),
            translate_with(MISSING_ITEMS, sample),
            Some(localize_missing_item(sample, crate::i18n::Locale::En)),
            Some(crate::catalog::definition::error_text(sample, crate::i18n::Locale::En)),
        ];
        if candidates.iter().flatten().any(|en| !has_cjk(en)) {
            Ok(())
        } else {
            Err(sentence)
        }
    }

    #[test]
    fn extras_read_failures_have_an_english_form() {
        for sample in [
            "读取扩展失败：permission denied",
            "读取序列失败：permission denied",
            "读取事件失败：permission denied",
            "读取分区失败：permission denied",
            "读取事件与分区失败：permission denied",
            "读取附加库失败：permission denied",
            "读取journal_mode失败：没有返回值",
            "读取枚举失败：permission denied",
            "读取域失败：permission denied",
        ] {
            let en = english(sample);
            assert!(!has_cjk(&en), "{sample} -> {en}");
        }
    }

    /// 确认层、对象定义查看器与结构页路径上（不含本文件与测试）的每个中文字面量都有英文：结构读取、执行与应用层
    /// 整个文件扫描；定义查看器扫描 `catalog/definition.rs` 与 `catalog/read.rs` 里定义读取用到的函数；外壳与结构页
    /// 扫描确认层/查看器的读取、显示函数与结构页源码。占位按样例值匹配。新加一句中文而没补英文，这个测试就失败。
    #[test]
    fn every_chinese_literal_on_ddl_paths_has_an_english_form() {
        const READ: &str = include_str!("../catalog/read.rs");
        const VIEW: &str = include_str!("../app/shell/view.rs");
        const CHROME: &str = include_str!("../app/shell/chrome.rs");
        const MODEL: &str = include_str!("../app/shell/model.rs");
        let mut sources: Vec<(String, String)> = [
            ("ddl/commands.rs", include_str!("commands.rs")),
            ("ddl/exec.rs", include_str!("exec.rs")),
            ("ddl/introspect.rs", include_str!("introspect.rs")),
            ("ddl/mod.rs", include_str!("mod.rs")),
            ("ddl/model.rs", include_str!("model.rs")),
            ("ddl/plan.rs", include_str!("plan.rs")),
            ("ddl/sql.rs", include_str!("sql.rs")),
            ("application/ddl.rs", include_str!("../application/ddl.rs")),
            ("catalog/definition.rs", include_str!("../catalog/definition.rs")),
            ("app/shell/danger.rs", include_str!("../app/shell/danger.rs")),
            ("app/views/structure/mod.rs", include_str!("../app/views/structure/mod.rs")),
        ]
        .into_iter()
        .map(|(file, source)| (file.to_owned(), non_test(source)))
        .collect();
        // 定义读取：入口、各引擎的定义读取与它们用到的取值/包装函数。
        for name in [
            "read_failure",
            "definition_failure",
            "read_definition",
            "read_sqlite_definition",
            "read_mysql_definition",
            "mysql_create_sql",
            "mysql_show_create_at",
            "mysql_string",
            "read_postgres_definition",
            "read_postgres_text",
            "pg_string",
            "tds_query_rows",
            "read_sqlserver_definition",
        ] {
            sources.push((format!("catalog/read.rs::{name}"), function_source(READ, name)));
        }
        // 外壳：查看器与确认层的读取回调、状态更新和渲染。
        for (file, source, name) in [
            ("app/shell/view.rs", VIEW, "spawn_definition"),
            ("app/shell/view.rs", VIEW, "pump_danger"),
            ("app/shell/model.rs", MODEL, "open_definition"),
            ("app/shell/model.rs", MODEL, "definition_loaded"),
            ("app/shell/chrome.rs", CHROME, "ddl_layer"),
            ("app/shell/chrome.rs", CHROME, "danger_layer"),
        ] {
            sources.push((format!("{file}::{name}"), function_source(source, name)));
        }
        let mut seen = Vec::new();
        let mut missing = Vec::new();
        for (file, code) in &sources {
            for literal in chinese_literals(code) {
                seen.push(literal.clone());
                if NOT_MESSAGES.iter().any(|(text, _)| *text == literal) {
                    continue;
                }
                if let Err(sentence) = has_english_form(&fill_placeholders(&literal)) {
                    missing.push(format!("{file}: {literal} -> {sentence}"));
                }
            }
        }
        assert!(seen.len() > 200, "扫描没有读到源码字面量：{}", seen.len());
        for file in ["catalog/definition.rs", "catalog/read.rs::read_sqlserver_definition", "app/shell/danger.rs"] {
            let (_, code) = sources.iter().find(|(name, _)| name == file).unwrap();
            assert!(!chinese_literals(code).is_empty(), "{file} 没有读到中文字面量，扫描范围可能失效");
        }
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "{} 句没有英文：\n{}", missing.len(), missing.join("\n"));
        for (text, reason) in NOT_MESSAGES {
            assert!(seen.iter().any(|literal| literal == text), "白名单项已不在源码里，应删除：{text}（{reason}）");
        }
    }

    #[test]
    fn literal_scan_skips_comments_and_reads_escapes() {
        let source = "// 注释 \"不算\"\nlet a = '\"'; let b: &'static str = \"找不到 {name}\\\"x\\\"\"; /* \"块注释\" */ let c = \"plain\";";
        assert_eq!(chinese_literals(source), vec!["找不到 {name}\\\"x\\\"".to_owned()]);
        assert_eq!(fill_placeholders("读不到 {name}：{err} {{x}} {:?}"), "读不到 v0：v1 {x} v2");
    }

    /// 确认层会显示的程序自己的错误（预览过期、连接未打开、后续跳过、兜底）都有英文。
    #[test]
    fn app_generated_errors_have_an_english_form() {
        for text in [
            "预览已过期：数据库里的结构或生成的 SQL 和确认时看到的不一致，没有执行。",
            "前面的语句没有成功，这条没有执行。",
            "这条连接还没有打开，没有另开新的连接。",
            "结构变更需要先确认预览中的 SQL，未确认不会执行。",
            "这个连接没有连着可查询的数据库。",
            "会话类型和目标数据库不一致，没有另开连接。",
            "foreign_key_check 发现 3 处违规，不会把这次重建当成成功。",
            "不支持的数据库 oracle",
            "没有执行任何语句",
            "生成计划失败",
            "执行失败",
        ] {
            let en = english(text);
            assert!(!has_cjk(&en), "没有英文：{text} -> {en}");
        }
    }
}
