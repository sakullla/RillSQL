//! PostgreSQL 专属说明句的英文：计划风险/阻断/步骤、读取与执行的错误、对象定义提示等只由这个引擎生成的中文句子。
//! 由 `EngineMeta::messages_en` 交给 `ddl::english`，先于共用句型查找；`{}` 等占位规则见 `ddl/english.rs`。

/// (中文句型, 英文句型)。同一前缀里更具体的句型排在前面。
pub(in crate::adapters) const MESSAGES_EN: &[(&str, &str)] = &[
    ("PostgreSQL 需要指定 schema（表所在的分组名），没有另开连接。", "PostgreSQL needs a schema (the group the table is in); no other connection was opened."),
    (
        "TRUNCATE TABLE 不触发逐行的 DELETE 触发器，不重置关联序列，也不加 CASCADE。",
        "TRUNCATE TABLE doesn't fire row-level DELETE triggers, doesn't reset owned sequences and doesn't add CASCADE.",
    ),
    ("{} 已经是 {} 的值，没有生成语句。", "{0} is already a value of {1}, so no statement was generated."),
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
    ("序列 {} 的参数读不到（可能是权限不足），没有生成语句。", "The settings of sequence {0} can't be read (possibly a missing privilege), so no statement was generated."),
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
    (
        "这些语句逐条执行，失败即停。已经成功的语句不撤销。PostgreSQL 虽然可以把结构变更放进事务，这里也不包成一个大事务，不能据此认为所有数据库都会回滚。",
        "These statements run one at a time and stop at the first failure. Statements that already succeeded are not undone. PostgreSQL could put structure changes in a transaction, but they are not wrapped in one here, so don't assume every database rolls back.",
    ),
    ("读不到序列 {}：{}", "Sequence {0} could not be read: {1}"),
    ("找不到序列 {}。", "Sequence {0} was not found."),
    ("读不到类型 {}：{}", "Type {0} could not be read: {1}"),
    ("找不到类型 {}。", "Type {0} was not found."),
    ("{} 不是枚举类型，枚举值只能加在枚举上。", "{0} is not an enum type; enum values can only be added to an enum."),
    ("读不到枚举 {} 的取值：{}", "The values of enum {0} could not be read: {1}"),
    ("{} 不是域（domain）。", "{0} is not a domain."),
    ("读不到域 {} 的定义：{}", "The definition of domain {0} could not be read: {1}"),
    ("找不到域 {}。", "Domain {0} was not found."),
    ("由唯一约束 {} 维护", "Maintained by unique constraint {0}"),
    ("修改类型 {}", "Change type of {0}"),
    ("允许 NULL {}", "Allow NULL on {0}"),
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
    ("PostgreSQL 需要 schema", "PostgreSQL needs a schema"),
    ("有多个同名函数，需要用参数类型区分", "Several functions have this name; their argument types are needed to tell them apart"),
    ("有多个同名触发器，需要所属的表才能区分", "Several triggers have this name; their table is needed to tell them apart"),
    (
        "只有视图、函数、存储过程、触发器、序列、枚举和域可以读取只读定义",
        "Only views, functions, stored procedures, triggers, sequences, enums and domains have a read-only definition",
    ),
];

/// 重建定义“未包含”清单里这个引擎专属的项。
pub(in crate::adapters) const MISSING_ITEMS_EN: &[(&str, &str)] = &[
    ("存储参数与表空间", "storage parameters and tablespace"),
    (
        "标识列的 ALWAYS / BY DEFAULT 模式与序列参数（统一写成 BY DEFAULT）",
        "the identity column's ALWAYS / BY DEFAULT mode and sequence options (written as BY DEFAULT)",
    ),
];
