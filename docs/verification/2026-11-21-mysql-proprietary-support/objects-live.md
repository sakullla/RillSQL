# MySQL / MariaDB 对象真实验收（T3：事件、函数/存储过程、触发器）

生成时间：2026-11-21 19:23:44 +0800

## 运行命令

```
python scripts/verify-mysql-objects.py --require-fixtures --report docs/verification/2026-11-21-mysql-proprietary-support/objects-live.md
cargo test --manifest-path src-tauri\Cargo.toml --locked --lib -- --ignored --nocapture object_lifecycle_round_trip_on_real_servers
```

## 实际运行版本

| 引擎 | 镜像 | 服务器版本 | event_scheduler | 用例是否执行 |
| --- | --- | --- | --- | --- |
| mysql | mysql:9.7.4 | 9.7.4 | ON | 是 |
| mariadb | mariadb:12.3.3 | 12.3.3-MariaDB-ubu2404 | OFF | 是 |

## 功能、实现路径与覆盖关系

| 功能 | 代码路径 | 元数据 / 驱动路径 | 本轮实际覆盖的行为 |
| --- | --- | --- | --- |
| 事件（单次 / 周期） | adapters/mysql/object_ddl.rs event_schedule_sql、event_statement；启停走 SetEventEnabled | information_schema.EVENTS（STATUS / EXECUTE_AT / INTERVAL_* / STARTS / ENDS / ON_COMPLETION / EVENT_DEFINITION）+ SHOW VARIABLES event_scheduler | 建单次与周期事件（EVERY 含 STARTS/ENDS）；改调度与主体；启用、停用、删除；每步读回调度、主体与状态；服务器调度器状态不改 |
| 函数 / 存储过程 | adapters/mysql/object_ddl.rs routine_statement、params_sql、definer_clause | information_schema.ROUTINES + PARAMETERS（按 ROUTINE_TYPE 过滤） | 建函数的参数/返回类型/多语句主体；替换改参数与主体并保留 DEFINER；存储过程 IN/OUT/INOUT；删除后读回不到 |
| 触发器 | adapters/mysql/object_ddl.rs trigger_statement、replace_steps | information_schema.TRIGGERS（ACTION_TIMING / EVENT_MANIPULATION / ACTION_STATEMENT）+ 一次真实 DML | 建 BEFORE INSERT 多语句主体触发器；INSERT 证明绑定并改写列；替换为 AFTER UPDATE 后 UPDATE 触发、INSERT 不再触发；删除后 DML 不再触发 |
| 同名函数与存储过程 | adapters/mysql/object_read.rs ROUTINE_SQL / PARAMETERS_SQL 的 ROUTINE_TYPE 条件 | information_schema.ROUTINES / PARAMETERS 的 ROUTINE_TYPE 列 | 夹具里同名函数与存储过程并存：参数与主体分别读回，不互相串（T1 交接修正的真机证据） |
| 失败路径与替换风险 | ddl/exec.rs 逐条执行与 receipts；object_ddl.rs 的 danger 风险与阻断原因 | 服务器原始错误 | 无效事件主体、重名事件、无效参数类型、目标表不存在、无效触发器主体各一次原始错误；替换中途 CREATE 失败时 DROP 已执行、对象读回不到 |

驱动：mysql_async 0.37.1（rustls）。MySQL 与 MariaDB 不互相替代：两个引擎各自跑完整流程。

## 覆盖关系（R9：最高版本实测 → 低版本按同一实现路径 + 版本门控推断）

每组只在实施时的**最高目标版本**上实测，更低版本不单独起容器：由高版本的结果加上同一实现路径与版本门控推断。推断项一律标为推断，不当成已验证。

| 家族 | 实测版本（服务器 VERSION()） | 本轮是否执行 | 与基线固定版本一致 | 由它推断的更低目标版本 |
| --- | --- | --- | --- | --- |
| MySQL | 9.7.4 | 是 | 是（9.7.4） | 8.4.13 / 8.0.46 / 5.7.44 |
| MariaDB | 12.3.3-MariaDB-ubu2404 | 是 | 是（12.3.3） | 11.8.9 / 11.4.13 / 10.11.19 / 10.6.28 |

方向只能是高 → 低：本轮用到的语法（CREATE/ALTER/DROP EVENT、CREATE/DROP FUNCTION 与 PROCEDURE、CREATE/DROP TRIGGER）与读回路径在 9.7.4 / 12.3.3 上全部跑通，更低目标版本的语法是它的子集；反过来，低版本上通过的结论证明不了高版本。所以本报告不拿低版本“代表”高版本。

共用同一实现路径的依据：

- DDL 生成：adapters/mysql/object_ddl.rs 对两个家族用同一份代码，没有按主次版本分支；版本门控只由 adapters/mysql/profile.rs 的 FamilyFeatures 决定（events / routines / triggers 在全部目标版本为真）。
- 元数据读回：adapters/mysql/object_read.rs 用 information_schema.EVENTS / ROUTINES / PARAMETERS / TRIGGERS，两条例程查询都按 ROUTINE_TYPE 过滤。
- DEFINER 保留：读回的是 information_schema 的 user@host 原文，替换时按标识符引号写回，两个家族写法一致。
- 驱动与协议：同一 mysql_async 0.37.1（rustls）；多语句主体作为同一条 Step.sql 发送，不按分号切分。

仍属推断 / 限制（不得当作已验证事实）：

- 上表“由它推断的更低目标版本”整列都是推断，本轮没有在这些版本上运行过。
- MySQL：夹具打开了 log_bin_trust_function_creators：binlog 打开时 MySQL 要求函数声明 DETERMINISTIC / NO SQL / READS SQL DATA，对象编辑器不替用户编这些语义，未声明的函数会被服务器拒绝（原始错误照实显示）。
- MySQL：8.4 / 8.0 / 5.7 上的事件与例程读写没有单独实测：按同一实现路径与同一 information_schema 列推断。
- MySQL：5.7 的 PARAMETERS 表在函数返回行上的写法没有单独核对，按 9.7.4 的读回结果推断。
- MySQL：9.x 的认证插件变化（如 mysql_native_password 移除）属于连接认证路径，不在本报告范围。
- MariaDB：夹具打开了 log_bin_trust_function_creators：binlog 打开时 MariaDB 同样要求函数声明 DETERMINISTIC / NO SQL / READS SQL DATA。
- MariaDB：11.x / 10.x 上的事件与例程读写没有单独实测：按同一实现路径推断。
- MariaDB：MariaDB 的 information_schema.ROUTINES.ROUTINE_DEFINITION 截断行为没有逐版本核对。
- 本报告只证明“实测版本”一列。Delivery 记录覆盖范围时必须照抄这里的分界，不能写成“已验证全部目标版本”。

## 用例证据（用例自身打印）

```
object-live: mysql event_scheduler 当前状态 Some(true)
object-live: mysql 建周期事件 -> CREATE EVENT rillsql_proprietary.t3_event_mysql_38248 ON SCHEDULE EVERY 1 DAY STARTS '2026-01-01 00:00:00' ON COMPLETION NOT PRESERVE DISABLE COMMENT 't3 周期事件' DO INSERT INTO audit_log (order_id, action) VALUES (0, 'tick')
object-live: mysql 周期事件读回 body="INSERT INTO audit_log (order_id, action) VALUES (0, 'tick')" enabled=false
object-live: mysql 改周期事件 -> ALTER EVENT rillsql_proprietary.t3_event_mysql_38248 ON SCHEDULE EVERY 2 HOUR ON COMPLETION PRESERVE DISABLE COMMENT 't3 周期事件' DO INSERT INTO audit_log (order_id, action) VALUES (0, 'tick2')
object-live: mysql 周期事件未写 STARTS，服务器补成 2026-11-21 11:23:42
object-live: mysql 启用事件提示：事件会按调度运行（服务器 event_scheduler 已打开）；本语句不改调度器状态。
object-live: mysql 建单次事件 -> CREATE EVENT rillsql_proprietary.t3_event_once_mysql_38248 ON SCHEDULE AT '2027-01-01 00:00:00' ON COMPLETION PRESERVE DISABLE DO INSERT INTO audit_log (order_id, action) VALUES (0, 'once')
object-live: mysql 删除事件后读回：找不到事件 rillsql_proprietary.t3_event_mysql_38248。
object-live: mysql 无效事件主体被服务器拒绝：ERROR 1064 (42000): You have an error in your SQL syntax; check the manual that corresponds to your MySQL server version for the right syntax to use near 'IS NOT SQL' at line 5
object-live: mysql 重名事件被服务器拒绝：ERROR 1537 (HY000): Event 't3_event_bad_mysql_38248' already exists
object-live: mysql 建函数 -> CREATE FUNCTION rillsql_proprietary.t3_function_mysql_38248(seed int, step int) RETURNS int COMMENT 't3 函数' BEGIN   DECLARE total INT;   SET total = seed * step;   RETURN total; END
object-live: mysql 函数读回 params=["seed", "step"] returns="int" body="BEGIN\n  DECLARE total INT;\n  SET total = seed * step;\n  RETURN total;\nEND"
object-live: mysql 替换函数 -> DROP FUNCTION rillsql_proprietary.t3_function_mysql_38248; | CREATE DEFINER=root@`%` FUNCTION rillsql_proprietary.t3_function_mysql_38248(seed int, step int, bonus int) RETURNS int COMMENT 't3 函数' BEGIN   DECLARE total INT;   SET total = (seed + bonus) * step;   RETURN total; END
object-live: mysql 建存储过程 -> CREATE PROCEDURE rillsql_proprietary.t3_procedure_mysql_38248(IN seed int, OUT total int, INOUT count int) BEGIN   SET total = seed * 2;   SET count = count + 1; END
object-live: mysql 存储过程读回参数模式 [In, Out, InOut]
object-live: mysql 函数与存储过程删除后都读回不到
object-live: mysql 同名函数/存储过程读回分开：函数 1 / 过程 2
object-live: mysql 无效参数类型被服务器拒绝：ERROR 1064 (42000): You have an error in your SQL syntax; check the manual that corresponds to your MySQL server version for the right syntax to use near 'not a type) RETURNS int
object-live: mysql 函数/存储过程互换被拒绝：找不到例程 rillsql_proprietary.existing_function。
object-live: mysql 替换中途失败：DROP=Ok CREATE=Failed，读回 "找不到例程 rillsql_proprietary.t3_function_gap_mysql_38248。"
object-live: mysql 建触发器 -> CREATE TRIGGER rillsql_proprietary.t3_trigger_mysql_38248 BEFORE INSERT ON rillsql_proprietary.orders FOR EACH ROW BEGIN   INSERT INTO audit_log (order_id, action) VALUES (NEW.id, 'insert');   SET NEW.note = 'before-insert'; END
object-live: mysql 触发器读回 Before Insert on orders body="BEGIN\n  INSERT INTO audit_log (order_id, action) VALUES (NEW.id, 'insert');\n  SET NEW.note = 'before-insert';\nEND"
object-live: mysql 触发器在 INSERT 上执行：audit_log +1，note 被改写
object-live: mysql 替换触发器 -> DROP TRIGGER rillsql_proprietary.t3_trigger_mysql_38248; | CREATE DEFINER=root@`%` TRIGGER rillsql_proprietary.t3_trigger_mysql_38248 AFTER UPDATE ON rillsql_proprietary.orders FOR EACH ROW BEGIN   INSERT INTO audit_log (order_id, action) VALUES (NEW.id, 'update'); END
object-live: mysql 替换后 UPDATE 触发、INSERT 不再触发
object-live: mysql 删除触发器后 DML 不再触发
object-live: mysql 目标表不存在被拒绝：ERROR 1146 (42S02): Table 'rillsql_proprietary.no_such_table' doesn't exist
object-live: mysql 无效触发器主体被拒绝：ERROR 1064 (42000): You have an error in your SQL syntax; check the manual that corresponds to your MySQL server version for the right syntax to use near 'IS NOT SQL' at line 2
object-live: mysql 全部用例完成
object-live: mariadb event_scheduler 当前状态 Some(false)
object-live: mariadb 建周期事件 -> CREATE EVENT rillsql_proprietary.t3_event_mariadb_38248 ON SCHEDULE EVERY 1 DAY STARTS '2026-01-01 00:00:00' ON COMPLETION NOT PRESERVE DISABLE COMMENT 't3 周期事件' DO INSERT INTO audit_log (order_id, action) VALUES (0, 'tick')
object-live: mariadb 周期事件读回 body="INSERT INTO audit_log (order_id, action) VALUES (0, 'tick')" enabled=false
object-live: mariadb 改周期事件 -> ALTER EVENT rillsql_proprietary.t3_event_mariadb_38248 ON SCHEDULE EVERY 2 HOUR ON COMPLETION PRESERVE DISABLE COMMENT 't3 周期事件' DO INSERT INTO audit_log (order_id, action) VALUES (0, 'tick2')
object-live: mariadb 周期事件未写 STARTS，服务器补成 2026-11-21 11:23:43
object-live: mariadb 启用事件提示：服务器 event_scheduler 现在是关闭的：事件启用后仍然不会运行，这里不改服务器级调度器状态。
object-live: mariadb 建单次事件 -> CREATE EVENT rillsql_proprietary.t3_event_once_mariadb_38248 ON SCHEDULE AT '2027-01-01 00:00:00' ON COMPLETION PRESERVE DISABLE DO INSERT INTO audit_log (order_id, action) VALUES (0, 'once')
object-live: mariadb 删除事件后读回：找不到事件 rillsql_proprietary.t3_event_mariadb_38248。
object-live: mariadb 无效事件主体被服务器拒绝：ERROR 1064 (42000): You have an error in your SQL syntax; check the manual that corresponds to your MariaDB server version for the right syntax to use near 'IS NOT SQL' at line 5
object-live: mariadb 重名事件被服务器拒绝：ERROR 1537 (HY000): Event 't3_event_bad_mariadb_38248' already exists
object-live: mariadb 建函数 -> CREATE FUNCTION rillsql_proprietary.t3_function_mariadb_38248(seed int, step int) RETURNS int COMMENT 't3 函数' BEGIN   DECLARE total INT;   SET total = seed * step;   RETURN total; END
object-live: mariadb 函数读回 params=["seed", "step"] returns="int(11)" body="BEGIN\n  DECLARE total INT;\n  SET total = seed * step;\n  RETURN total;\nEND"
object-live: mariadb 替换函数 -> DROP FUNCTION rillsql_proprietary.t3_function_mariadb_38248; | CREATE DEFINER=root@`%` FUNCTION rillsql_proprietary.t3_function_mariadb_38248(seed int, step int, bonus int) RETURNS int COMMENT 't3 函数' BEGIN   DECLARE total INT;   SET total = (seed + bonus) * step;   RETURN total; END
object-live: mariadb 建存储过程 -> CREATE PROCEDURE rillsql_proprietary.t3_procedure_mariadb_38248(IN seed int, OUT total int, INOUT count int) BEGIN   SET total = seed * 2;   SET count = count + 1; END
object-live: mariadb 存储过程读回参数模式 [In, Out, InOut]
object-live: mariadb 函数与存储过程删除后都读回不到
object-live: mariadb 同名函数/存储过程读回分开：函数 1 / 过程 2
object-live: mariadb 无效参数类型被服务器拒绝：ERROR 1064 (42000): You have an error in your SQL syntax; check the manual that corresponds to your MariaDB server version for the right syntax to use near 'not a type) RETURNS int
object-live: mariadb 函数/存储过程互换被拒绝：找不到例程 rillsql_proprietary.existing_function。
object-live: mariadb 替换中途失败：DROP=Ok CREATE=Failed，读回 "找不到例程 rillsql_proprietary.t3_function_gap_mariadb_38248。"
object-live: mariadb 建触发器 -> CREATE TRIGGER rillsql_proprietary.t3_trigger_mariadb_38248 BEFORE INSERT ON rillsql_proprietary.orders FOR EACH ROW BEGIN   INSERT INTO audit_log (order_id, action) VALUES (NEW.id, 'insert');   SET NEW.note = 'before-insert'; END
object-live: mariadb 触发器读回 Before Insert on orders body="BEGIN\n  INSERT INTO audit_log (order_id, action) VALUES (NEW.id, 'insert');\n  SET NEW.note = 'before-insert';\nEND"
object-live: mariadb 触发器在 INSERT 上执行：audit_log +1，note 被改写
object-live: mariadb 替换触发器 -> DROP TRIGGER rillsql_proprietary.t3_trigger_mariadb_38248; | CREATE DEFINER=root@`%` TRIGGER rillsql_proprietary.t3_trigger_mariadb_38248 AFTER UPDATE ON rillsql_proprietary.orders FOR EACH ROW BEGIN   INSERT INTO audit_log (order_id, action) VALUES (NEW.id, 'update'); END
object-live: mariadb 替换后 UPDATE 触发、INSERT 不再触发
object-live: mariadb 删除触发器后 DML 不再触发
object-live: mariadb 目标表不存在被拒绝：ERROR 1146 (42S02): Table 'rillsql_proprietary.no_such_table' doesn't exist
object-live: mariadb 无效触发器主体被拒绝：ERROR 1064 (42000): You have an error in your SQL syntax; check the manual that corresponds to your MariaDB server version for the right syntax to use near 'IS NOT SQL' at line 2
object-live: mariadb 全部用例完成
```

## 测试结果与原始输出（末尾 80 行）

```
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1650 filtered out; finished in 1.25s
running 1 test
object-live: mysql event_scheduler 当前状态 Some(true)
object-live: mysql 建周期事件 -> CREATE EVENT rillsql_proprietary.t3_event_mysql_38248 ON SCHEDULE EVERY 1 DAY STARTS '2026-01-01 00:00:00' ON COMPLETION NOT PRESERVE DISABLE COMMENT 't3 周期事件' DO INSERT INTO audit_log (order_id, action) VALUES (0, 'tick')
object-live: mysql 周期事件读回 body="INSERT INTO audit_log (order_id, action) VALUES (0, 'tick')" enabled=false
object-live: mysql 改周期事件 -> ALTER EVENT rillsql_proprietary.t3_event_mysql_38248 ON SCHEDULE EVERY 2 HOUR ON COMPLETION PRESERVE DISABLE COMMENT 't3 周期事件' DO INSERT INTO audit_log (order_id, action) VALUES (0, 'tick2')
object-live: mysql 周期事件未写 STARTS，服务器补成 2026-11-21 11:23:42
object-live: mysql 启用事件提示：事件会按调度运行（服务器 event_scheduler 已打开）；本语句不改调度器状态。
object-live: mysql 建单次事件 -> CREATE EVENT rillsql_proprietary.t3_event_once_mysql_38248 ON SCHEDULE AT '2027-01-01 00:00:00' ON COMPLETION PRESERVE DISABLE DO INSERT INTO audit_log (order_id, action) VALUES (0, 'once')
object-live: mysql 删除事件后读回：找不到事件 rillsql_proprietary.t3_event_mysql_38248。
object-live: mysql 无效事件主体被服务器拒绝：ERROR 1064 (42000): You have an error in your SQL syntax; check the manual that corresponds to your MySQL server version for the right syntax to use near 'IS NOT SQL' at line 5
object-live: mysql 重名事件被服务器拒绝：ERROR 1537 (HY000): Event 't3_event_bad_mysql_38248' already exists
object-live: mysql 建函数 -> CREATE FUNCTION rillsql_proprietary.t3_function_mysql_38248(seed int, step int) RETURNS int COMMENT 't3 函数' BEGIN   DECLARE total INT;   SET total = seed * step;   RETURN total; END
object-live: mysql 函数读回 params=["seed", "step"] returns="int" body="BEGIN\n  DECLARE total INT;\n  SET total = seed * step;\n  RETURN total;\nEND"
object-live: mysql 替换函数 -> DROP FUNCTION rillsql_proprietary.t3_function_mysql_38248; | CREATE DEFINER=root@`%` FUNCTION rillsql_proprietary.t3_function_mysql_38248(seed int, step int, bonus int) RETURNS int COMMENT 't3 函数' BEGIN   DECLARE total INT;   SET total = (seed + bonus) * step;   RETURN total; END
object-live: mysql 建存储过程 -> CREATE PROCEDURE rillsql_proprietary.t3_procedure_mysql_38248(IN seed int, OUT total int, INOUT count int) BEGIN   SET total = seed * 2;   SET count = count + 1; END
object-live: mysql 存储过程读回参数模式 [In, Out, InOut]
object-live: mysql 函数与存储过程删除后都读回不到
object-live: mysql 同名函数/存储过程读回分开：函数 1 / 过程 2
object-live: mysql 无效参数类型被服务器拒绝：ERROR 1064 (42000): You have an error in your SQL syntax; check the manual that corresponds to your MySQL server version for the right syntax to use near 'not a type) RETURNS int
RETURN seed' at line 1
object-live: mysql 函数/存储过程互换被拒绝：找不到例程 rillsql_proprietary.existing_function。
object-live: mysql 替换中途失败：DROP=Ok CREATE=Failed，读回 "找不到例程 rillsql_proprietary.t3_function_gap_mysql_38248。"
object-live: mysql 建触发器 -> CREATE TRIGGER rillsql_proprietary.t3_trigger_mysql_38248 BEFORE INSERT ON rillsql_proprietary.orders FOR EACH ROW BEGIN   INSERT INTO audit_log (order_id, action) VALUES (NEW.id, 'insert');   SET NEW.note = 'before-insert'; END
object-live: mysql 触发器读回 Before Insert on orders body="BEGIN\n  INSERT INTO audit_log (order_id, action) VALUES (NEW.id, 'insert');\n  SET NEW.note = 'before-insert';\nEND"
object-live: mysql 触发器在 INSERT 上执行：audit_log +1，note 被改写
object-live: mysql 替换触发器 -> DROP TRIGGER rillsql_proprietary.t3_trigger_mysql_38248; | CREATE DEFINER=root@`%` TRIGGER rillsql_proprietary.t3_trigger_mysql_38248 AFTER UPDATE ON rillsql_proprietary.orders FOR EACH ROW BEGIN   INSERT INTO audit_log (order_id, action) VALUES (NEW.id, 'update'); END
object-live: mysql 替换后 UPDATE 触发、INSERT 不再触发
object-live: mysql 删除触发器后 DML 不再触发
object-live: mysql 目标表不存在被拒绝：ERROR 1146 (42S02): Table 'rillsql_proprietary.no_such_table' doesn't exist
object-live: mysql 无效触发器主体被拒绝：ERROR 1064 (42000): You have an error in your SQL syntax; check the manual that corresponds to your MySQL server version for the right syntax to use near 'IS NOT SQL' at line 2
object-live: mysql 全部用例完成
object-live: mariadb event_scheduler 当前状态 Some(false)
object-live: mariadb 建周期事件 -> CREATE EVENT rillsql_proprietary.t3_event_mariadb_38248 ON SCHEDULE EVERY 1 DAY STARTS '2026-01-01 00:00:00' ON COMPLETION NOT PRESERVE DISABLE COMMENT 't3 周期事件' DO INSERT INTO audit_log (order_id, action) VALUES (0, 'tick')
object-live: mariadb 周期事件读回 body="INSERT INTO audit_log (order_id, action) VALUES (0, 'tick')" enabled=false
object-live: mariadb 改周期事件 -> ALTER EVENT rillsql_proprietary.t3_event_mariadb_38248 ON SCHEDULE EVERY 2 HOUR ON COMPLETION PRESERVE DISABLE COMMENT 't3 周期事件' DO INSERT INTO audit_log (order_id, action) VALUES (0, 'tick2')
object-live: mariadb 周期事件未写 STARTS，服务器补成 2026-11-21 11:23:43
object-live: mariadb 启用事件提示：服务器 event_scheduler 现在是关闭的：事件启用后仍然不会运行，这里不改服务器级调度器状态。
object-live: mariadb 建单次事件 -> CREATE EVENT rillsql_proprietary.t3_event_once_mariadb_38248 ON SCHEDULE AT '2027-01-01 00:00:00' ON COMPLETION PRESERVE DISABLE DO INSERT INTO audit_log (order_id, action) VALUES (0, 'once')
object-live: mariadb 删除事件后读回：找不到事件 rillsql_proprietary.t3_event_mariadb_38248。
object-live: mariadb 无效事件主体被服务器拒绝：ERROR 1064 (42000): You have an error in your SQL syntax; check the manual that corresponds to your MariaDB server version for the right syntax to use near 'IS NOT SQL' at line 5
object-live: mariadb 重名事件被服务器拒绝：ERROR 1537 (HY000): Event 't3_event_bad_mariadb_38248' already exists
object-live: mariadb 建函数 -> CREATE FUNCTION rillsql_proprietary.t3_function_mariadb_38248(seed int, step int) RETURNS int COMMENT 't3 函数' BEGIN   DECLARE total INT;   SET total = seed * step;   RETURN total; END
object-live: mariadb 函数读回 params=["seed", "step"] returns="int(11)" body="BEGIN\n  DECLARE total INT;\n  SET total = seed * step;\n  RETURN total;\nEND"
object-live: mariadb 替换函数 -> DROP FUNCTION rillsql_proprietary.t3_function_mariadb_38248; | CREATE DEFINER=root@`%` FUNCTION rillsql_proprietary.t3_function_mariadb_38248(seed int, step int, bonus int) RETURNS int COMMENT 't3 函数' BEGIN   DECLARE total INT;   SET total = (seed + bonus) * step;   RETURN total; END
object-live: mariadb 建存储过程 -> CREATE PROCEDURE rillsql_proprietary.t3_procedure_mariadb_38248(IN seed int, OUT total int, INOUT count int) BEGIN   SET total = seed * 2;   SET count = count + 1; END
object-live: mariadb 存储过程读回参数模式 [In, Out, InOut]
object-live: mariadb 函数与存储过程删除后都读回不到
object-live: mariadb 同名函数/存储过程读回分开：函数 1 / 过程 2
object-live: mariadb 无效参数类型被服务器拒绝：ERROR 1064 (42000): You have an error in your SQL syntax; check the manual that corresponds to your MariaDB server version for the right syntax to use near 'not a type) RETURNS int
RETURN seed' at line 1
object-live: mariadb 函数/存储过程互换被拒绝：找不到例程 rillsql_proprietary.existing_function。
object-live: mariadb 替换中途失败：DROP=Ok CREATE=Failed，读回 "找不到例程 rillsql_proprietary.t3_function_gap_mariadb_38248。"
object-live: mariadb 建触发器 -> CREATE TRIGGER rillsql_proprietary.t3_trigger_mariadb_38248 BEFORE INSERT ON rillsql_proprietary.orders FOR EACH ROW BEGIN   INSERT INTO audit_log (order_id, action) VALUES (NEW.id, 'insert');   SET NEW.note = 'before-insert'; END
object-live: mariadb 触发器读回 Before Insert on orders body="BEGIN\n  INSERT INTO audit_log (order_id, action) VALUES (NEW.id, 'insert');\n  SET NEW.note = 'before-insert';\nEND"
object-live: mariadb 触发器在 INSERT 上执行：audit_log +1，note 被改写
object-live: mariadb 替换触发器 -> DROP TRIGGER rillsql_proprietary.t3_trigger_mariadb_38248; | CREATE DEFINER=root@`%` TRIGGER rillsql_proprietary.t3_trigger_mariadb_38248 AFTER UPDATE ON rillsql_proprietary.orders FOR EACH ROW BEGIN   INSERT INTO audit_log (order_id, action) VALUES (NEW.id, 'update'); END
object-live: mariadb 替换后 UPDATE 触发、INSERT 不再触发
object-live: mariadb 删除触发器后 DML 不再触发
object-live: mariadb 目标表不存在被拒绝：ERROR 1146 (42S02): Table 'rillsql_proprietary.no_such_table' doesn't exist
object-live: mariadb 无效触发器主体被拒绝：ERROR 1064 (42000): You have an error in your SQL syntax; check the manual that corresponds to your MariaDB server version for the right syntax to use near 'IS NOT SQL' at line 2
object-live: mariadb 全部用例完成
test adapters::mysql::object_live_tests::object_lifecycle_round_trip_on_real_servers ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1650 filtered out; finished in 1.25s

    Blocking waiting for file lock on build directory
   Compiling rillsql v0.3.1 (C:\Users\12976\project\RillSQL\src-tauri)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 4m 53s
     Running unittests src\lib.rs (C:/Users/12976/.cargo/target\debug\deps\rillsql_lib-b488e4f6c69edc93.exe)
```
