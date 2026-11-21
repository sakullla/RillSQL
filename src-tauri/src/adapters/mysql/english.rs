//! MySQL / MariaDB 专属说明句的英文：计划风险/阻断/步骤、读取与执行的错误、对象定义提示等只由这个引擎生成的中文句子。
//! 由 `EngineMeta::messages_en` 交给 `ddl::english`，先于共用句型查找；`{}` 等占位规则见 `ddl/english.rs`。

/// (中文句型, 英文句型)。同一前缀里更具体的句型排在前面。
pub(in crate::adapters) const MESSAGES_EN: &[(&str, &str)] = &[
    ("MySQL 要写成带括号的表达式 ({})。", "MySQL needs this written as a parenthesized expression ({0})."),
    ("列类型 {} 的当前时间默认值要写 {}（小数秒位数必须一致）。", "The current-time default for column type {0} must be written {1} (the fractional-second precision must match)."),
    (
        "TRUNCATE TABLE 会立即提交并把 AUTO_INCREMENT 重置到初始值，不触发 DELETE 触发器。",
        "TRUNCATE TABLE commits immediately and resets AUTO_INCREMENT to its initial value; DELETE triggers don't fire.",
    ),
    (
        "表 {} 使用 MyISAM，不支持外键。不会通过关闭检查来假装创建成功。",
        "Table {0} uses MyISAM, which doesn't support foreign keys. Checks are not turned off to pretend the creation succeeded.",
    ),
    (
        "这些语句逐条执行，失败即停。MySQL 与 MariaDB 会立即提交每条结构变更语句，中途失败不会撤销已经成功的语句，不能声称已经全部回滚。",
        "These statements run one at a time and stop at the first failure. MySQL and MariaDB commit each structure change immediately, so a failure midway doesn't undo the statements that already succeeded; nothing can be claimed as fully rolled back.",
    ),
    ("只改事件的启停状态，不改它的事件体。", "Only the event's enabled state changes; its body stays the same."),
    ("只有 MySQL 与 MariaDB 有事件（event）对象，这里不生成语句。", "Only MySQL and MariaDB have event objects, so no statement is generated here."),
    ("事件名无效。", "The event name is not valid."),
    (
        "服务器 event_scheduler 现在是关闭的：事件启用后仍然不会运行，这里不改服务器级调度器状态。",
        "event_scheduler is currently off on the server: the event still won't run after being enabled, and the server-level scheduler state is not changed here.",
    ),
    (
        "事件会按调度运行（服务器 event_scheduler 已打开）；本语句不改调度器状态。",
        "The event will run on its schedule (event_scheduler is on); this statement doesn't change the scheduler state.",
    ),
    (
        "没有读到 event_scheduler 的实际状态：启用的事件只有在它打开时才会运行，这里不改服务器级状态。",
        "The actual event_scheduler state could not be read: an enabled event runs only while it is on, and the server-level state is not changed here.",
    ),
    (
        "事件保存为停用状态，不会运行；这里不改服务器级 event_scheduler 状态。",
        "The event is saved disabled and will not run; the server-level event_scheduler state is not changed here.",
    ),
    (
        "ALTER EVENT 只改这个事件的定义与状态，不启动或停止服务器调度器。",
        "ALTER EVENT only changes this event's definition and state; it doesn't start or stop the server scheduler.",
    ),
    ("启用事件 {}", "Enable event {0}"),
    ("停用事件 {}", "Disable event {0}"),
    ("读不到事件 {}：{}", "Event {0} could not be read: {1}"),
    ("找不到事件 {}。", "Event {0} was not found."),
    ("没有读到建表语句", "The CREATE TABLE statement could not be read"),
    ("MariaDB 没有独立的枚举或域", "MariaDB has no separate enums or domains"),
    ("MySQL 没有独立的序列、枚举或域", "MySQL has no separate sequences, enums or domains"),
    ("只有表、视图、函数、存储过程、触发器和事件可以读取只读定义", "Only tables, views, functions, stored procedures, triggers and events have a read-only definition"),
    ("元数据列缺失", "A metadata column is missing"),
];

/// 重建定义“未包含”清单里这个引擎专属的项。
pub(in crate::adapters) const MISSING_ITEMS_EN: &[(&str, &str)] = &[];
