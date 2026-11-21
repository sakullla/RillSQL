# 全库执行风险真实验收（T4）

生成时间：2026-11-21 22:52:18 +0800

## 运行命令

```
python scripts/verify-execution-risk.py --require-fixtures --keep --report docs/verification/2026-11-21-mysql-proprietary-support/execution-risk-live.md
cargo test --manifest-path src-tauri\Cargo.toml --locked --lib -- --ignored --nocapture execution_risk_on_real_fixtures
```

## 实际运行版本与执行情况

| 引擎 | 版本 | 用例是否执行 |
| --- | --- | --- |
| postgres | PostgreSQL 16.16 (Debian 16.16-1.pgdg13+1) on x86_64-pc-linux-gnu, compiled by gcc (Debian 14.2.0-19) 14.2.0, 64-bit | 是 |
| mysql | 8.4.13 | 是 |
| mariadb | 11.4.13-MariaDB-ubu2404 | 是 |
| sqlserver | Microsoft SQL Server 2022 (RTM-CU28) (KB5125472) - 16.0.4325.4 (X64) Nov 4 2026 14:09:11 Copyright (C) 2022 Microsoft Corporation Developer Edition (64-bit) on  | 是 |
| redis | redis_version:7.4.12 | 是 |
| mongo | 8.3.17 | 是 |
| sqlite | 3.51.3 | 是 |

## 功能、实现路径与真实证据

| 引擎 | 覆盖的操作 | 代码路径 | 实测到的后果 |
| --- | --- | --- | --- |
| postgres | SELECT/EXPLAIN 直通；DROP TABLE、整表 UPDATE、多语句混合 | adapters/postgres/exec.rs（逐条发送）+ query/risk.rs 的后果分类 | DROP TABLE 判为数据丢失；表真的消失；整表 UPDATE 判为大量重写 |
| mysql | DROP TABLE、整表 UPDATE、多语句混合 | adapters/mysql/exec.rs；DDL 逐条执行、无整体回滚 | DROP TABLE 判为数据丢失 + 隐式提交；多语句只拦风险那条 |
| mariadb | DROP TABLE、整表 UPDATE、多语句混合 | adapters/mariadb/exec.rs（与 MySQL 同族不同实现路径） | 同上；MariaDB 独立实测，不用 MySQL 结果替代 |
| sqlite | DELETE 无 WHERE、多语句混合 | adapters/sqlite/exec.rs；文件库（临时目录） | 整表 DELETE 判为数据丢失；SQLite 没有 TRUNCATE，用全表删除对照 |
| sqlserver | TRUNCATE TABLE、整表 UPDATE、多语句混合 | adapters/sqlserver/exec.rs（TDS 批量） | TRUNCATE 判为数据丢失；没有隐式提交说明（SQL Server 可回滚） |
| redis | FLUSHDB 未确认、DBSIZE 只读 | adapters/redis/exec.rs 的 CommandRisk + application/redis.rs 守卫 | Blocked/Dangerous/Readonly 不变；未确认的写命令在应用层被拒 |
| mongo | deleteOne 未确认、find 只读 | application/mongo.rs 的 docs::require_delete 守卫 | 未确认删除被拒；读操作不受影响 |

判定口径：风险分类来自 `query/risk.rs`（纯函数，按后果分档），执行前决策来自 `query/desk.rs` 的 `begin_run`，
确认后由同一批 `PreparedRun` 经 `commands/exec` 打到服务器；用例没有绕过这两层直接发 SQL。

## 路径差异与限制

- SQLite 是临时文件夹具，不是 compose 服务：验证的是同一份 sqlite 适配器与执行链。
- Redis 与 MongoDB 不是 SQL 通道：报告只证明其既有守卫没有回退，风险分类仍留在各自适配器与应用层。
- MySQL 与 MariaDB 各自实跑，不互相替代；两者版本号见「实际运行版本」。
- SQL Server 只跑 2022-latest 一个镜像；T-SQL 的其它版本没有单独实测。
- 客户端可判断的后果（数据丢失、对象替换、大量重写、隐式提交、外部调度）在执行前提示；服务端才能发现的错误仍保留原始错误信息。

## 用例证据（用例自身打印）

```
execution-risk: postgres 只读对照 SELECT 1 -> ok 1 行
execution-risk: postgres 风险操作 DROP TABLE -> ok 0 行受影响 后果 [DataLoss]
execution-risk: postgres 删表后读回 -> error 0 行受影响
execution-risk: postgres 风险操作 UPDATE risk_drop_9272_rows SET note = 'x' -> ok 2 行受影响 后果 [MassRewrite]
execution-risk: postgres 多语句混合 -> 确认 1 条 / 直通 2 条 -> ok 1 行 / ok 0 行受影响 / ok 1 行
execution-risk: postgres 全部用例完成
execution-risk: mysql 只读对照 SELECT 1 -> ok 1 行
execution-risk: mysql 风险操作 DROP TABLE -> ok 0 行受影响 后果 [DataLoss, ImplicitCommit]
execution-risk: mysql 删表后读回 -> error 0 行受影响
execution-risk: mysql 风险操作 UPDATE risk_drop_9272_rows SET note = 'x' -> ok 2 行受影响 后果 [MassRewrite]
execution-risk: mysql 多语句混合 -> 确认 1 条 / 直通 2 条 -> ok 1 行 / ok 0 行受影响 / ok 1 行
execution-risk: mysql 全部用例完成
execution-risk: mariadb 只读对照 SELECT 1 -> ok 1 行
execution-risk: mariadb 风险操作 DROP TABLE -> ok 0 行受影响 后果 [DataLoss, ImplicitCommit]
execution-risk: mariadb 删表后读回 -> error 0 行受影响
execution-risk: mariadb 风险操作 UPDATE risk_drop_9272_rows SET note = 'x' -> ok 2 行受影响 后果 [MassRewrite]
execution-risk: mariadb 多语句混合 -> 确认 1 条 / 直通 2 条 -> ok 1 行 / ok 0 行受影响 / ok 1 行
execution-risk: mariadb 全部用例完成
execution-risk: sqlserver 只读对照 SELECT 1 -> ok 1 行
execution-risk: sqlserver 风险操作 DROP TABLE -> ok 0 行受影响 后果 [DataLoss]
execution-risk: sqlserver 删表后读回 -> error 0 行受影响
execution-risk: sqlserver 风险操作 TRUNCATE TABLE risk_drop_9272_rows -> ok 0 行受影响 后果 [DataLoss]
execution-risk: sqlserver 多语句混合 -> 确认 1 条 / 直通 2 条 -> ok 1 行 / ok 0 行受影响 / ok 1 行
execution-risk: sqlserver 全部用例完成
execution-risk: redis 只读对照 DBSIZE -> 成功
execution-risk: redis 风险操作 FLUSHDB（未确认）-> 被应用层拒绝：FLUSHDB 属于写删类命令，需要先在界面确认。
execution-risk: redis 全部用例完成
execution-risk: mongo 只读对照 listDatabases -> 成功
execution-risk: mongo 风险操作 deleteOne（未确认）-> 被应用层拒绝：删除文档属于写删操作，需要先在界面确认。
execution-risk: mongo 全部用例完成
execution-risk: sqlite 版本 3.51.3
execution-risk: sqlite 只读对照 SELECT 1 -> ok 1 行
execution-risk: sqlite 风险操作 DROP TABLE -> ok 0 行受影响 后果 [DataLoss]
execution-risk: sqlite 删表后读回 -> error 0 行受影响
execution-risk: sqlite 风险操作 DELETE FROM risk_drop_9272_rows -> ok 2 行受影响 后果 [DataLoss]
execution-risk: sqlite 多语句混合 -> 确认 1 条 / 直通 2 条 -> ok 1 行 / ok 0 行受影响 / ok 1 行
execution-risk: sqlite 全部用例完成
```

## 测试结果与原始输出（末尾 80 行）

```
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1667 filtered out; finished in 5.47s
running 1 test
execution-risk: sqlite 版本 3.51.3
execution-risk: postgres 只读对照 SELECT 1 -> ok 1 行
execution-risk: postgres 风险操作 DROP TABLE -> ok 0 行受影响 后果 [DataLoss]
execution-risk: postgres 删表后读回 -> error 0 行受影响
execution-risk: postgres 风险操作 UPDATE risk_drop_9272_rows SET note = 'x' -> ok 2 行受影响 后果 [MassRewrite]
execution-risk: postgres 多语句混合 -> 确认 1 条 / 直通 2 条 -> ok 1 行 / ok 0 行受影响 / ok 1 行
execution-risk: postgres 全部用例完成
execution-risk: mysql 只读对照 SELECT 1 -> ok 1 行
execution-risk: mysql 风险操作 DROP TABLE -> ok 0 行受影响 后果 [DataLoss, ImplicitCommit]
execution-risk: mysql 删表后读回 -> error 0 行受影响
execution-risk: mysql 风险操作 UPDATE risk_drop_9272_rows SET note = 'x' -> ok 2 行受影响 后果 [MassRewrite]
execution-risk: mysql 多语句混合 -> 确认 1 条 / 直通 2 条 -> ok 1 行 / ok 0 行受影响 / ok 1 行
execution-risk: mysql 全部用例完成
execution-risk: mariadb 只读对照 SELECT 1 -> ok 1 行
execution-risk: mariadb 风险操作 DROP TABLE -> ok 0 行受影响 后果 [DataLoss, ImplicitCommit]
execution-risk: mariadb 删表后读回 -> error 0 行受影响
execution-risk: mariadb 风险操作 UPDATE risk_drop_9272_rows SET note = 'x' -> ok 2 行受影响 后果 [MassRewrite]
execution-risk: mariadb 多语句混合 -> 确认 1 条 / 直通 2 条 -> ok 1 行 / ok 0 行受影响 / ok 1 行
execution-risk: mariadb 全部用例完成
execution-risk: sqlserver 只读对照 SELECT 1 -> ok 1 行
execution-risk: sqlserver 风险操作 DROP TABLE -> ok 0 行受影响 后果 [DataLoss]
execution-risk: sqlserver 删表后读回 -> error 0 行受影响
execution-risk: sqlserver 风险操作 TRUNCATE TABLE risk_drop_9272_rows -> ok 0 行受影响 后果 [DataLoss]
execution-risk: sqlserver 多语句混合 -> 确认 1 条 / 直通 2 条 -> ok 1 行 / ok 0 行受影响 / ok 1 行
execution-risk: sqlserver 全部用例完成
execution-risk: sqlite 只读对照 SELECT 1 -> ok 1 行
execution-risk: sqlite 风险操作 DROP TABLE -> ok 0 行受影响 后果 [DataLoss]
execution-risk: sqlite 删表后读回 -> error 0 行受影响
execution-risk: sqlite 风险操作 DELETE FROM risk_drop_9272_rows -> ok 2 行受影响 后果 [DataLoss]
execution-risk: sqlite 多语句混合 -> 确认 1 条 / 直通 2 条 -> ok 1 行 / ok 0 行受影响 / ok 1 行
execution-risk: sqlite 全部用例完成
execution-risk: redis 只读对照 DBSIZE -> 成功
execution-risk: redis 风险操作 FLUSHDB（未确认）-> 被应用层拒绝：FLUSHDB 属于写删类命令，需要先在界面确认。
execution-risk: redis 全部用例完成
execution-risk: mongo 只读对照 listDatabases -> 成功
execution-risk: mongo 风险操作 deleteOne（未确认）-> 被应用层拒绝：删除文档属于写删操作，需要先在界面确认。
execution-risk: mongo 全部用例完成
test query::risk_live_tests::execution_risk_on_real_fixtures ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1667 filtered out; finished in 5.47s

    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.06s
     Running unittests src\lib.rs (C:/Users/12976/.cargo/target\debug\deps\rillsql_lib-b488e4f6c69edc93.exe)
```
