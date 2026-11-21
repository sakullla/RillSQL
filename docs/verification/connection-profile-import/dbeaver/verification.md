# DBeaver 连接导入验收

## 来源与复测输入

2026-11-21 在 Windows 上运行 DBeaver Community Edition 26.2.5，通过专用工作区的连接创建和编辑界面、启动参数生成 7 条连接。安装包的 `.eclipseproduct` 记录了构建号 `26.2.5-202611151759`、提交 `78bc3cc`，与 `org.jkiss.dbeaver.ce.feature_26.2.5.202611151759` 一致。输入取自该项目的 `General/.dbeaver/data-sources.json`。[脱敏样本](data-sources.json)

脱敏时保留了驱动 ID、连接类型、处理器、数值类型及 JDBC 与离散地址的冲突。两条 SQLite 连接改用专用目录 `C:/Temp/rillsql-import-fixture/`；PostgreSQL URL 中的密码替换为 `RILLSQL_SYNTHETIC_PASSWORD_01`，只用于检验泄漏。样本只包含连接定义。原文件 SHA-256 为 `495cece91982e1f2ea1cd9ccc4b33039fdce7c386b2097377611e0e41a66befa`，脱敏文件为 `f1e586709a08b0c0741f87e97dee5a0b00fdb62b9fa082b9216f51fcb2003cf6`。

| 样本连接 | 应核对的目标与条件 |
| --- | --- |
| Import Fixture SQLite | 从普通 database 字段取 `rillsql-import-demo.sqlite` 文件。 |
| DBeaver Sample Database (SQLite) | 从 SQLite JDBC URL 取 `Chinook.db` 文件。 |
| Import Fixture PostgreSQL | 单跳 SSH 到 `localhost:22`；来源将端口写作 `22.0`、额外跳板数写作 `0.0`。缺少 SSH 用户及数据库用户名，需要补齐。 |
| Import Fixture PostgreSQL URL Verify | URL 指向 `127.0.0.1:5432/rillsql`，要求 `verify-full`；预览须保留有效目标，并说明当前 PostgreSQL 适配器的 TLS 限制。 |
| Import Fixture MySQL | `127.0.0.1:3306/rillsql` 明确指定只读，须阻止保存。 |
| Import Fixture PostgreSQL URL Plain | 离散字段为 `wrong-target.invalid:5999/unused`，有效数据库为 `127.0.0.1:5432/rillsql`，用户名 `postgres`，URL 显式关闭 TLS。 |
| Import Fixture Oracle | 未支持的驱动，报告须保留该行及原因。 |

复测时准备专用 RillSQL 数据目录，并在上述测试路径放置可辨识的 SQLite 文件。在连接管理页选择 DBeaver 和本目录的 JSON。先取消并核对新增数，再重新选择文件、部分勾选、确认并重开连接列表。复测 PostgreSQL 时通过现有凭据入口填写本地测试库认证信息。

## Windows 桌面取证

本轮验收的 RillSQL 可执行文件 SHA-256 为 `377a04df07d272a089e98e4a57086002c7a763b097a4a3e04f3f812ae2a02723`。所有以下操作均经原生连接管理页发起，使用独立配置库。

| 桌面操作 | 观测结果 |
| --- | --- |
| 选取脱敏样本并取消，重新打开后只勾选 PostgreSQL URL Plain | 取消后实际新增 0。第二次报告成功 1、跳过 6、失败 0；其中 2 条 SQLite 未勾选，SSH 条目缺少用户，Verify 条目要求当前 PostgreSQL 驱动无法还原的 TLS 校验，只读 MySQL 和 Oracle 分别列明限制与不支持原因。保存后的地址为 `127.0.0.1:5432/rillsql`，用户 `postgres`，TLS 模式 `off`。[预览](preview.png) · [逐条报告](report-one.png) |
| 再次选取同一来源行并按默认勾选确认 | 该行显示已存在，成功 0、跳过 1、失败 0。[重复导入](repeat-report.png) |
| 同名来源行的 JDBC 地址改为 `127.0.0.1:5433/rillsql_other` | 预览给出原目标与新目标，默认跳过。改名为 DBeaver Renamed PostgreSQL 后成功新增 1 条，配置库中的原行仍指向端口 5432。[同名冲突](same-name.png) · [改名另存](rename-report.png) |
| 从真实 SSH 条目派生单行样本，将环境设为 `prod`，补齐数据库用户名与 SSH 用户 | 预览保留来源的 `localhost:22` 隧道，写入数据库用户 `postgres`、跳板用户 `fixture-user` 和环境标签 `prod`，两个已存密码标志均为 0。以上导入操作结束后，PostgreSQL 的 RillSQL 会话数为 0。[SSH 补齐后预览](ssh-ready.png) |
| 选择损坏的 DBeaver JSON | 出现文件级解析错误，显示已保存 0 条；已有 3 条连接保留。[文件错误](file-error.png) |

检查上述配置库、应用状态、查询历史、标准输出和标准错误文件，均未找到样本 URL 的合成密码标记。预览与逐条报告的截图也未显示该标记。

同一可执行文件另从 RillSQL 原有导入入口回归原生包。明文包使配置库从 3 条增至 4 条；选择加密包后出现口令提示，输入错误口令仍为 4 条，输入正确口令增至 5 条。选择被截断的加密包提示格式错误，仍为 5 条。桌面加密夹具按 RillSQL 格式参数和合成口令生成；项目另有 Rust 编解码往返测试。[错误口令](bundle-wrong.png) · [损坏加密包](bundle-damaged.png) · [原生格式测试](../../../../src-tauri/src/connections/export.rs)

重开完整的 7 行脱敏样本，导入剩余两条合格的 SQLite 连接后，本地库由 5 条增至 7 条。再选同一文件并以默认选择确认，报告成功 0、跳过 7、失败 0，配置库仍为 7 条。[整份文件重复导入](full-repeat-report.png)

在连接列表主动打开 PostgreSQL URL Plain，通过现有口令提示登录。容器端 `pg_stat_activity` 返回 `rillsql|postgres|RillSQL`。随后从列表打开 Import Fixture SQLite，界面显示已连接。本地配置库保存的路径为 `C:/Temp/rillsql-import-fixture/rillsql-import-demo.sqlite`；从该文件查询 `SELECT * FROM fixture_marker` 得到一行 `rillsql-import-fixture`。[PostgreSQL 连接状态](target-connected.png) · [SQLite 连接状态](sqlite-connected.png)

### 首轮候选的连接验证

在候选 `1657e9e` 上从专用 DBeaver 工作区首次导入，取消后配置库计数为 0。再次导入只选择 Import Fixture SQLite 与 Import Fixture PostgreSQL URL Plain，7 条中保存 2 条。关闭报告、重开连接列表后，从 RillSQL 本地 SQLite 配置库核对到两条记录，PostgreSQL 的主机为 `127.0.0.1`、端口 `5432`、数据库 `rillsql`、用户 `postgres`、TLS 模式 `off`，两条记录的数据库和 SSH 已存密码标志均为 0。

另行点击该 PostgreSQL 连接，在既有密码提示中输入本地测试库密码并关闭记住密码。RillSQL 显示已连接，数据库容器的 `pg_stat_activity` 显示数据库 `rillsql`、用户 `postgres`、应用 `RillSQL`。[PostgreSQL 实际连接](postgres-connected-1657e9e.png)

## Rust 回归覆盖

桌面任务的模型测试以临时 SQLite 触发器构造单条写入失败，断言报告为成功 1、跳过 2、失败 1，随后撤销触发器重试，断言成功 1、跳过 3、失败 0，最终实际新增 2 条。存储层另有跨服务并发、同名改名、已有凭据保护、生产标签失效和失败重试测试。解析层覆盖带密码的 JDBC URL、字段和调试输出脱敏、生产环境的继承限制以及证书和 SSH 设置。[桌面任务测试](../../../../src-tauri/src/app/shell/tests.rs) · [持久化测试](../../../../src-tauri/src/connections/tests.rs) · [DBeaver 解析测试](../../../../src-tauri/src/connections/profile_import/tests.rs)
