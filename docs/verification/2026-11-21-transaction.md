# 事务完全支持：实库与 Windows 原生验收（T6）

日期：2026-11-22
范围：R1、R2、R4、R6、R7、R8、R9、R10、R11 的实库与原生证据。
代码基线：`ae560e0`（T1–T5 已完成；本任务只新增 `src-tauri/src/transaction/live_tests.rs`、在 `src-tauri/src/transaction/mod.rs` 声明测试模块并生成验证产物，未改业务实现）。

产物位置：本文件、`docs/verification/2026-11-21-transaction/live.txt`、`native-*.png` 与 `native-build.txt`。当前 `.gitignore` 忽略 `docs/verification/*`，提交时需要按既有验证目录的方式显式加入。

## 结论

- **五种关系型数据库的同一事务行为契约全部通过**：PostgreSQL 16.16、MySQL 8.4.13、MariaDB 11.4.13、SQLite 3.51.3、SQL Server 2022（16.0.4325.4）。每个引擎都用适配器真实的方言 SQL 执行开始、DML、提交、回滚、保存点/回滚到保存点、释放保存点（SQL Server 明确不支持）、隔离级别设置、错误后恢复，并用独立连接核对提交/回滚结果。
- **PostgreSQL 的控制响应丢失验证通过**：COMMIT 与 ROLLBACK 已转发、响应未返回时，客户端得到 `connection closed`；方言与执行层都把结果登记为 `Unconfirmed(ConnectionLost)`；代理只看到一次控制语句，等待 500 ms 后没有重放。
- **MySQL/MariaDB 的控制响应丢失验证发现实现缺口**：协议层事实全部成立（只转发一次、客户端得到错误、没有重放），但执行层把丢失的 COMMIT/ROLLBACK 登记为 `Rejected`（事务保持 `Active`），而不是 `Unconfirmed(ConnectionLost)`。详见“已知缺口”。因此 R10 在 MySQL/MariaDB 上的“提交结果未知”尚未获得通过证据，不能记为通过。
- **Windows 原生**：构建、启动、`BEGIN` 后共享事务条、跨标签仍显示活动事务、关闭标签不结束事务、活动事务中 `Alt+F4` 出现关闭保护对话框，均有截图。网格“应用 vs 提交”与保护对话框按钮后的继续流程未在原生窗口完成，按未验证记录。

| 引擎 | 提交/回滚 | 保存点 | 隔离 | 错误恢复 | 隐式提交 | 控制响应丢失 |
|---|---|---|---|---|---|---|
| PostgreSQL 16.16 | 通过 | 通过 | 通过（serializable） | 通过（Aborted + 保存点恢复 + 中止 COMMIT 实际回滚） | 不适用 | 通过（未知 + 不重放） |
| MySQL 8.4.13 | 通过 | 通过 | 通过（SERIALIZABLE） | 通过（事务仍可用） | 通过 | 协议通过；**应用映射未通过（GAP）** |
| MariaDB 11.4.13 | 通过 | 通过 | 通过（SERIALIZABLE） | 通过（事务仍可用） | 通过 | 协议通过；**应用映射未通过（GAP）** |
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

原始输出：`docs/verification/2026-11-21-transaction/live.txt`（8 个 ignored 测试：6 通过，2 失败，失败项都是 MySQL/MariaDB 的未知映射断言）。

新增测试文件通过 Clippy（`cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked`，0 警告；输出 `docs/verification/2026-11-21-transaction/clippy.txt`）。`cargo fmt --check` 当前仍会在未改动的 `src-tauri/src/ui/tabs.rs` 等文件报格式差异，与本任务新增文件无关，交由 Delivery 处理。

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

- PostgreSQL：COMMIT 与 ROLLBACK 均为 `failure=ConnectionLost`、`app_outcome=Unconfirmed(ConnectionLost)`；客户端错误 `connection closed`；独立核对 COMMIT 行存在、ROLLBACK 行不存在（对客户端仍是未知）。
- MySQL/MariaDB：客户端错误 `Input/output error: Input/output error: connection closed`；代理只看到一次控制语句；没有重放。但 `is_write("COMMIT")=false` 使 `classify_failure` 走 `StatementStop::Error`，MySQL 方言的 `is_connection_failure` 又未匹配 `connection closed`，应用结局为 `Rejected`。见缺口。

## 已知缺口

### G1（阻塞 R10 的 MySQL/MariaDB 未知证据）

- 现象：COMMIT/ROLLBACK 响应丢失后，客户端得到连接错误，但执行层登记为 `Rejected`，事务仍显示 `Active`，不是“结果未知”。
- 证据：`live.txt` 中 `engine=MySQL/MariaDB check=proxy_unknown_mapping GAP app_outcome=Rejected app_failure=TransactionUsable path=dialect_error is_write=false`。
- 原因链：
  - `src-tauri/src/query/exec.rs:148` 只在 `is_write(sql)` 为真时把断连错误折成 `StatementStop::Unknown`；
  - `src-tauri/src/query/split.rs:110-133` 的 `is_write` 不包含 `COMMIT`/`ROLLBACK`；
  - 因此控制语句走 `StatementStop::Error`，再由 `control_result` 调 `StatementFailure`；
  - `src-tauri/src/adapters/mysql/transaction.rs:143-151` 的 `is_connection_failure` 未包含 `connection closed`（驱动真实文本），返回 `TransactionUsable` → `Rejected`。
- 影响：`transaction::execute` 对丢失的提交/回滚返回 `Err`，界面先显示“数据库拒绝”，只有等连接轮询/重连把旧事务登记为未知后才出现未知提示；R10 要求的“提交结果无法确认时显示未知”没有在控制结果处成立。
- 建议（未实现，T6 不改业务代码）：让控制语句的断连也进入 `StatementStop::Unknown`（例如把事务控制视为需要确认的操作），并在 MySQL 方言的连接失败文本中覆盖 `connection closed`/`forcibly closed`。修复后重跑本文件两个代理测试即可。

### G2（原生网格与保护后续流程未完成）

跨标签状态、关闭标签保留事务、关闭保护提示已由原生截图证明；网格“应用变更 vs 提交事务”和保护对话框按钮后的继续流程未在原生窗口完成，不能记为原生通过。见“Windows 原生证据 / 原生未完成项”。

## Windows 原生证据

- 构建命令：`cargo build --manifest-path src-tauri/Cargo.toml --bin rillsql --locked`
- 构建结果：`dev` profile 2m24s，退出码 0；原始输出 `docs/verification/2026-11-21-transaction/native-build.txt`。
- 启动：直接运行 `C:/Users/12976/.cargo/target/debug/rillsql.exe`（PID 47460），窗口标题 `RillSQL`，最大化为 2560×1440；截图 `native-launch.png`、`native-workbench.png`。
- 恢复的 SQLite 连接 `thread_history_1.sqlite`（`main` 库，5 张表）。在查询编辑器输入 `BEGIN` 并点击“运行”，结果区显示 `BEGIN ok`，共享事务条显示“事务进行中”、`提交事务`、`回滚事务`、`保存点`、`隔离级别`、`事务模式`。截图：`native-cross-tab.png`（该图同时包含后续步骤）。
- 跨标签：`Ctrl+T` 新建“查询 2”，事务条仍显示“事务进行中”，新标签直接看到同一连接事务。截图：`native-cross-tab.png`。
- 关闭标签：`Ctrl+W` 关闭“查询 2”，回到“查询 1”后事务条仍为“事务进行中”，证明关闭单个标签不结束连接事务（R11 第一条）。截图：`native-close-tab-keeps-tx.png`。
- 关闭保护：活动事务中按 `Alt+F4`，应用没有退出，弹出“退出前还有未结束的事务”，写明连接名与三条选择“取消 / 回滚后继续 / 提交后继续”。截图：`native-close-guard.png`。
- 收尾：证据采集后强制结束该调试进程；SQLite 连接关闭时数据库自行回滚未提交事务，未向产品代码或用户数据写入验证用修改。

### 原生未完成项

- **网格“应用变更”与“提交事务”的区分**：未在原生窗口完成表格编辑/应用/提交的人工路径；已有 GPUI 测试（`grid_edits_block_close_and_structure_admin_bind_the_target` 等）覆盖，但不能替代原生人工证据，按“未验证”记录。
- **原生保存点/隔离级别点击**：事务条入口可见，但本次未把创建保存点、切换隔离级别的完整点击路径纳入截图。
- **“回滚后继续 / 提交后继续”的实际继续路径**：关闭保护对话框已出现，未在原生窗口完成按钮点击后的继续/中止流程核对；由 `quitting_with_an_active_transaction_asks_first_and_only_continues_after_rollback` 等 GPUI 测试覆盖。
- 以上项不得据现有截图记为原生通过。

## 夹具限制

- `mssql-init` 在已有数据卷上重跑会因 `customers` 已存在而退出码 1；`rillsql` 库与种子表实际已存在，本任务的测试只使用自建临时表，不受影响。全新卷首次初始化不在本次证据内。
- 服务重启后 `docker compose up -d` 会重建容器；本次实测时五个服务均健康。
- SQLite 用临时文件，结束后删除；不使用 `docker/sqlite/notes.sqlite` 替代其他引擎。
