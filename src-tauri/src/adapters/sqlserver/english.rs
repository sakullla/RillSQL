//! SQL Server 专属说明句的英文：计划风险/阻断/步骤、读取与执行的错误、对象定义提示等只由这个引擎生成的中文句子。
//! 由 `EngineMeta::messages_en` 交给 `ddl::english`，先于共用句型查找；`{}` 等占位规则见 `ddl/english.rs`。

/// (中文句型, 英文句型)。同一前缀里更具体的句型排在前面。
pub(in crate::adapters) const MESSAGES_EN: &[(&str, &str)] = &[
    ("TRUNCATE TABLE 会重置 IDENTITY 计数，不触发 DELETE 触发器。", "TRUNCATE TABLE resets the IDENTITY counter; DELETE triggers don't fire."),
    ("sp_rename 不会改写引用旧名的存储过程、函数和视图。", "sp_rename doesn't update stored procedures, functions or views that use the old name."),
    ("SQL Server 需要指定 schema（表所在的分组名），没有另开连接。", "SQL Server needs a schema (the group the table is in); no other connection was opened."),
    (
        "这些语句逐条执行，失败即停。已成功的语句不撤销；这里也不把整批包进一个事务，不能据此声称可以整体回滚。",
        "These statements run one at a time and stop at the first failure. Statements that already succeeded are not undone, and the batch is not wrapped in one transaction, so it can't be claimed to roll back as a whole.",
    ),
    ("删除默认值 {}", "Drop default of {0}"),
    ("删除原默认值 {}", "Drop the old default of {0}"),
    ("SQL Server 需要 schema", "SQL Server needs a schema"),
    ("有多个同名对象，需要更具体的名称", "Several objects have this name; a more specific name is needed"),
    ("没有读到序列起点", "The sequence start value was not returned"),
    ("没有读到序列步长", "The sequence increment was not returned"),
    ("没有读到序列下限", "The sequence minimum was not returned"),
    ("没有读到序列上限", "The sequence maximum was not returned"),
    ("SQL Server 没有独立的枚举或域", "SQL Server has no separate enums or domains"),
    ("只有函数、存储过程、触发器、视图和序列可以读取只读定义", "Only functions, stored procedures, triggers, views and sequences have a read-only definition"),
];

/// 重建定义“未包含”清单里这个引擎专属的项。
pub(in crate::adapters) const MISSING_ITEMS_EN: &[(&str, &str)] = &[
    ("IDENTITY 的起点与步长（统一写成 1, 1）", "the IDENTITY seed and increment (written as 1, 1)"),
    ("计算列表达式", "computed column expressions"),
    ("文件组与压缩选项", "filegroup and compression options"),
    ("主键的 CLUSTERED / NONCLUSTERED 选项（按 SQL Server 默认处理）", "the primary key's CLUSTERED / NONCLUSTERED option (SQL Server's default is assumed)"),
    ("索引选项（FILLFACTOR、IGNORE_DUP_KEY 等）", "index options (FILLFACTOR, IGNORE_DUP_KEY and so on)"),
    ("XML 与空间索引", "XML and spatial indexes"),
];
