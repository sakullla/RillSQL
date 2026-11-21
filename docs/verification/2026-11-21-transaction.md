# 事务完全支持：实库与 Windows 原生验收（T6）

日期：2026-11-22
范围：R1、R2、R4、R6、R7、R8、R9、R10、R11 的实库与原生证据。
代码基线：`ae560e0`；T6-A1 证据提交 `85dab7c`。A1 只新增 `src-tauri/src/transaction/live_tests.rs` 并在 `src-tauri/src/transaction/mod.rs` 声明测试模块；A2 修复 R10 控制断连映射，改了 `src-tauri/src/query/{exec,split,tests}.rs` 与 `src-tauri/src/adapters/mysql/transaction.rs`，其余行为未变。

产物位置：本文件、`docs/verification/2026-11-21-transaction/live.txt`、`tests.txt`、`clippy.txt`、`fmt.txt`、`native-build.txt` 与 `native-*.png`。`.gitignore` 已对本目录与本文档重新包含，产物可直接进入检查点。

## 结论

- **五种关系型数据库的同一事务行为契约全部通过**：PostgreSQL 16.16、MySQL 8.4.13、MariaDB 11.4.13、SQLite 3.51.3、SQL Server 2022（16.0.4325.4）。每个引擎都用适配器真实的方言 SQL 执行开始、DML、提交、回滚、保存点/回滚到保存点、释放保存点（SQL Server 明确不支持）、隔离级别设置、错误后恢复，并用独立连接核对提交/回滚结果。
- **PostgreSQL 的控制响应丢失验证通过**：COMMIT 与 ROLLBACK 已转发、响应未返回时，客户端得到 `connection closed`；方言与执行层都把结果登记为 `Unconfirmed(ConnectionLost)`；代理只看到一次控制语句，等待 500 ms 后没有重放。
- **MySQL/MariaDB 的控制响应丢失验证已修复并通过**：A1 暴露的缺口（丢失 COMMIT/ROLLBACK 被登记为 `Rejected`）在 A2 修复；现在客户端得到 `connection closed`，执行层与方言层都登记 `Unconfirmed(ConnectionLost)`，代理只看到一次控制语句且没有重放。修复位置与回归见“R10 修复”。
- **Windows 原生**：构建、启动、`BEGIN` 后共享事务条、跨标签仍显示活动事务、关闭标签不结束事务、活动事务中 `Alt+F4` 出现关闭保护对话框，均有截图。网格“应用 vs 提交”与保护对话框按钮后的继续流程未在原生窗口完成，按未验证记录。

| 引擎 | 提交/回滚 | 保存点 | 隔离 | 错误恢复 | 隐式提交 | 控制响应丢失 |
|---|---|---|---|---|---|---|
| PostgreSQL 16.16 | 通过 | 通过 | 通过（serializable） | 通过（Aborted + 保存点恢复 + 中止 COMMIT 实际回滚） | 不适用 | 通过（未知 + 不重放） |
| MySQL 8.4.13 | 通过 | 通过 | 通过（SERIALIZABLE） | 通过（事务仍可用） | 通过 | 通过（未知 + 不重放，A2 修复后） |
| MariaDB 11.4.13 | 通过 | 通过 | 通过（SERIALIZABLE） | 通过（事务仍可用） | 通过 | 通过（未知 + 不重放，A2 修复后） |
| SQLite 3.51.3 | 通过 | 通过 | 通过（read_uncommitted pragma） | 通过（事务仍可用） | 不适用 | 未做（无网络控制通道） |
| SQL Server 2022 | 通过 | 通过（无 RELEASE，入口明确 Unsupported） | 通过（serializable） | 通过（XACT_STATE 仍为 1） | 不适用 | 未做（TDS 代理不在本次范围） |

## 夹具与命令

夹具来自 `docker/compose.yaml`（本机 Docker 29.9.3）：

```text
docker compose -f docker/compose.yaml up -d postgres mysql mariadb mssql mssql-init
```

| 服务 | 地址 | 账号/库 |
|---|---|---|
| PostgreSQL 16.16 | `127.0.0.1:5432` | `postgres/postgres`，库 `rillsql` |
| MySQL 8.4.13 | `127.0.0.1:3306` | `rillsql/rillsql`，库 `rillsql` |
| MariaDB 11.4.13 | `127.0.0.1:3307` | `rillsql/rillsql`，库 `rillsql` |
| SQL Server 2022 | `127.0.0.1:1433` | `sa/Rillsql-Passw0rd!`，库 `rillsql` |
| SQLite | 临时文件 | 无 |

实测命令与原始输出：

```text
cargo test --manifest-path src-tauri/Cargo.toml --locked transaction::live_tests -- --ignored --nocapture
```

原始输出：`docs/verification/2026-11-21-transaction/live.txt`（UTF-8 无 BOM；8 个 ignored 测试全部通过）。SQL Server 的 `release_savepoint` 仍按方言能力输出 `SKIP`，不是失败。

普通测试与静态检查：

```text
cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
```

`tests.txt` 记录普通测试：1715 passed、1 failed、19 ignored；新增的 8 个 live 测试全部仍为 ignored。唯一失败是 `ui::read_table::tests::click_shift_click_and_cmd_c_copy_the_block`（`src-tauri/src/ui/read_table.rs:940`，剪贴板为空），与事务/查询分类改动没有调用关系，单独复现仍失败；同一模块的右键复制测试通过，本任务未改该文件，按既有失败记录，不冒充通过。`clippy.txt` 记录 0 警告。`fmt.txt` 记录 `cargo fmt --check` 的基线失败：当前仓库在未改动的 `src-tauri/src/bin/rillsql-package.rs` 等文件即有格式差异，A2 没有做全仓重排，避免覆盖 sibling 改动。

端口可用 `RILLSQL_LIVE_PG_PORT`、`RILLSQL_LIVE_MYSQL_PORT`、`RILLSQL_LIVE_MARIADB_PORT`、`RILLSQL_LIVE_MSSQL_PORT` 及各 `_USER`/`_PASSWORD`/`_DATABASE` 覆盖；默认值就是 compose 固定值。

## 五引擎行为契约

每个引擎运行同一份 `run_contract`，检查项与输出一一对应：

1. `commit_uncommitted_invisible`：事务内 INSERT 后，独立连接看不到未提交行（PostgreSQL/MySQL/MariaDB/SQLite/SQL Server 均为 `observer_rows=0`）。
2. `commit_persists`：COMMIT 后独立连接看到该行（五个引擎均为 `observer_rows=1`）。
3. `rollback_discards`：ROLLBACK 后独立连接看不到该行（均为 0）。
4. `savepoint_partial_rollback`：保存点前 A 保留、保存点后 B 撤销，外层事务仍活动，最终提交只保留 A（五个引擎均为 `kept_a=1 undone_b=0`）。
5. `release_savepoint_keeps_transaction`：PostgreSQL/MySQL/MariaDB/SQLite 释放保存点后事务仍活动；SQL Server 记录 `SKIP`，方言把 `RELEASE SAVEPOINT` 明确判为 `Unsupported`（只有 `SAVE TRANSACTION`）。
6. `isolation_applied`：用方言 SQL 设置后，会话真实设置与所选值一致。PostgreSQL `serializable`；MySQL/MariaDB `SERIALIZABLE`；SQLite `read_uncommitted=1`；SQL Server `serializable`。
7. 错误恢复：
   - PostgreSQL：缺失表返回 `42P01`，随后普通语句得到 `25P02`（中止）；回滚到保存点后恢复并提交；中止事务的 COMMIT 由 `control_success(..., "ROLLBACK")` 登记为实际回滚，独立连接确认未持久化。
   - MySQL/MariaDB：缺失表返回 `1146`，事务仍可用，随后 ROLLBACK 正常。
   - SQLite：错误码 `1`，事务仍可用。
   - SQL Server：错误码 `208`，`XACT_STATE()` 探针仍为 `1`（活动），回滚后 `trancount=0 state=0`。
8. `implicit_commit_persists_ddl`（MySQL/MariaDB）：事务内 INSERT 后执行 `CREATE TABLE`，方言判定为 `Yes`，状态机登记 `ImplicitCommit` 并回到 `Idle`；随后 ROLLBACK 不再撤销该行（`rows_after_rollback=1`）。普通 INSERT 不误报隐式提交。

SQL Server 额外用方言探针 `SELECT @@TRANCOUNT AS trancount, XACT_STATE() AS state` 验证共享状态模型：活动时 `[[Some("1"), Some("1")]]`，回滚后 `[[Some("0"), Some("0")]]`。

## 控制响应丢失代理

测试在进程内启动最小 TCP 代理，按协议识别客户端控制帧（PostgreSQL `Q`、MySQL `COM_QUERY`）：

1. 转发 COMMIT/ROLLBACK 到真实数据库；
2. 在服务器响应返回客户端之前关闭连接；
3. 断言客户端收到错误、同一连接不能继续使用；
4. 断言代理只看到一次该控制语句，等待 500 ms 后没有重放；
5. 用应用真实的 `query::exec::classify_failure` 与方言 `control_failure` 复现执行层结局；
6. 用独立直连核对数据库事实，但不把“读到/没读到”冒充为客户端已知结果。

结果：

- PostgreSQL：COMMIT 与 ROLLBACK 均为 `failure=ConnectionLost`、`app_path=dialect_error`、`app_outcome=Unconfirmed(ConnectionLost)`；客户端错误 `connection closed`；代理只看到一次控制语句且无重放。独立核对 COMMIT 行存在、ROLLBACK 行不存在（对客户端仍是未知）。
- MySQL/MariaDB：客户端错误 `Input/output error: Input/output error: connection closed`；代理只看到一次控制语句且无重放。A2 后 `app_path=disconnect_unknown`、`app_outcome=Unconfirmed(ConnectionLost)`；方言路径也同时返回 `ConnectionLost`，两层一致。
- 三个代理测试的 `proxy_outcome_summary` 均为 `mapped_unknown=2/2`，`live.txt` 中不再有 `proxy_unknown_mapping GAP`。

## R10 修复（A2）

A1 暴露的阻断是：控制语句 COMMIT/ROLLBACK 已转发、响应丢失时，MySQL/MariaDB 被登记为 `Rejected`（事务仍 `Active`），不是 `Unconfirmed(ConnectionLost)`。A2 按最小改动修复两处，形成双层保障：

1. `src-tauri/src/query/split.rs` 新增 `is_transaction_control(sql)`：按首个代码关键字识别 `BEGIN/START/COMMIT/END/ROLLBACK/ABORT/SAVEPOINT/RELEASE`，只服务断连分类，不改变语句发送或安全判断。
2. `src-tauri/src/query/exec.rs` 的 `classify_failure`：断连时条件从 `is_write(sql)` 改为 `is_write(sql) || is_transaction_control(sql)`。因此控制语句断连返回 `StatementStop::Unknown`，再由 `query/transaction.rs` 的控制路径映射为 `Unconfirmed(ConnectionLost)`。
3. `src-tauri/src/adapters/mysql/transaction.rs` 的 `is_connection_failure` 补充驱动真实文本：`connection closed`、`connection is closed`、`forcibly closed`、`server closed`；控制失败经默认 `control_failure` 返回 `Unconfirmed(ConnectionLost)`。普通 SQL 语法错误仍返回 `TransactionUsable` 与 `Rejected`。

回归：

- `query::tests::disconnected_transaction_controls_are_unknown_not_ordinary_errors`：COMMIT/ROLLBACK/SAVEPOINT/RELEASE/BEGIN/START/END 的断连均为 Unknown；只读断连与 COMMIT 语法错误仍为 Error。
- `adapters::mysql::transaction::tests::connection_and_deadlock_errors_are_kept_apart`：真实断连文本均为 ConnectionLost；控制失败映射 Unknown，普通失败仍 Rejected。
- `transaction::live_tests::{postgres,mysql,mariadb}_control_response_loss_is_unknown_and_not_replayed`：真实夹具代理，三个引擎 `mapped_unknown=2/2` 且无重放。

## 剩余边界

跨标签状态、关闭标签保留事务、关闭保护提示已由原生截图证明；网格“应用变更 vs 提交事务”和保护对话框按钮后的继续流程未在原生窗口完成，不能记为原生通过。见“Windows 原生证据 / 原生未完成项”。

## Windows 原生证据

以下原生截图在 A1 代码（`85dab7c`）上采集；A2 只改控制断连的错误分类，不涉及 UI 或事务条绘制。A2 的二进制构建由普通 `cargo test`/`clippy` 覆盖，未重新拍摄原生截图。

- 构建命令：`cargo build --manifest-path src-tauri/Cargo.toml --bin rillsql --locked`
- 构建结果：`dev` profile 2m24s，退出码 0；原始输出 `docs/verification/2026-11-21-transaction/native-build.txt`。
- 启动：直接运行 `C:/Users/12976/.cargo/target/debug/rillsql.exe`（PID 47460），窗口标题 `RillSQL`，最大化为 2560×1440；截图 `native-launch.png`、`native-workbench.png`。
- 恢复的 SQLite 连接 `thread_history_1.sqlite`（`main` 库，5 张表）。在查询编辑器输入 `BEGIN` 并点击“运行”，结果区显示 `BEGIN ok`，共享事务条显示“事务进行中”、`提交事务`、`回滚事务`、`保存点`、`隔离级别`、`事务模式`。截图：`native-cross-tab.png`（该图同时包含后续步骤）。
- 跨标签：`Ctrl+T` 新建“查询 2”，事务条仍显示“事务进行中”，新标签直接看到同一连接事务。截图：`native-cross-tab.png`。
- 关闭标签：`Ctrl+W` 关闭“查询 2”，回到“查询 1”后事务条仍为“事务进行中”，证明关闭单个标签不结束连接事务（R11 第一条）。截图：`native-close-tab-keeps-tx.png`。
- 关闭保护：活动事务中按 `Alt+F4`，应用没有退出，弹出“退出前还有未结束的事务”，写明连接名与三条选择“取消 / 回滚后继续 / 提交后继续”。截图：`native-close-guard.png`。
- 收尾：证据采集后强制结束该调试进程；SQLite 连接关闭时数据库自行回滚未提交事务，未向产品代码或用户数据写入验证用修改。

截图索引：

| 文件 | 对应事实 |
|---|---|
| `native-launch.png` | 调试二进制启动后的 RillSQL 窗口 |
| `native-workbench.png` | 恢复 SQLite 连接后的工作台初始状态 |
| `native-cross-tab.png` | `BEGIN` 后事务条活动，且 `Ctrl+T` 的第二个查询标签仍显示同一事务 |
| `native-close-tab-keeps-tx.png` | `Ctrl+W` 关闭第二个标签后，剩余标签的事务仍活动 |
| `native-close-guard.png` | 活动事务中 `Alt+F4` 弹出关闭保护，提供取消/回滚后继续/提交后继续 |

### 原生未完成项

- **网格“应用变更”与“提交事务”的区分**：未在原生窗口完成表格编辑/应用/提交的人工路径；已有 GPUI 测试（`grid_edits_block_close_and_structure_admin_bind_the_target` 等）覆盖，但不能替代原生人工证据，按“未验证”记录。
- **原生保存点/隔离级别点击**：事务条入口可见，但本次未把创建保存点、切换隔离级别的完整点击路径纳入截图。
- **“回滚后继续 / 提交后继续”的实际继续路径**：关闭保护对话框已出现，未在原生窗口完成按钮点击后的继续/中止流程核对；由 `quitting_with_an_active_transaction_asks_first_and_only_continues_after_rollback` 等 GPUI 测试覆盖。
- 以上项不得据现有截图记为原生通过。

## 夹具限制

- `mssql-init` 在已有数据卷上重跑会因 `customers` 已存在而退出码 1；`rillsql` 库与种子表实际已存在，本任务的测试只使用自建临时表，不受影响。全新卷首次初始化不在本次证据内。
- 服务重启后 `docker compose up -d` 会重建容器；本次实测时五个服务均健康。
- SQLite 用临时文件，结束后删除；不使用 `docker/sqlite/notes.sqlite` 替代其他引擎。
