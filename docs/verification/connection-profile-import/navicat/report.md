# Navicat 连接导入验证

## 来源样本

[connections.ncx](connections.ncx) 来自 Windows 上的 Navicat Premium 18.0.8（文件版本 18.0.8.0）。在空白连接列表中新建四条测试配置，通过 **File → Export Connections** 全选导出，保持 Export Password 关闭。脱敏时清空 SettingsSavePath，将 SQLite 文件路径替换为本地验收夹具。保留其余连接属性和原生 UTF-8 NCX 1.5 结构。

| 导出条目 | 源目标与访问设置 |
| --- | --- |
| NCX Fixture PostgreSQL | `127.0.0.1:5432/rillsql`，用户 `postgres`，SSL 关闭 |
| NCX Fixture SQLite | `C:/Temp/rillsql-import-fixture/rillsql-import-demo.sqlite` |
| NCX Fixture MySQL SSH | `db.example.invalid:3306`，经 `fixture@bastion.example.invalid:2222` 单跳，SSH 密码认证 |
| NCX Fixture MySQL HTTP | `db.example.invalid:3306`，源文件启用 HTTP 隧道 |

当前适用范围为上述 NCX 版本和配置。解析器逐行识别 MySQL、PostgreSQL 和 SQLite，遇到 HTTP 隧道、未验证 SSL 条件、强制只读、启动语句或影响访问的未知选项时显示阻止原因。整文件加密、其他编码和其他 NCX 版本需另取真实导出验证。

DBX 的公开实现提供了字段交叉核对线索。此次兼容范围以本次 Navicat 导出为依据。[DBX 导入文档](https://dbxio.com/cn/docs/connection-import) · [DBX 字段映射源码](https://github.com/t8y2/dbx/blob/cbb05f90bd2aff1b401c569b15a3f9c891ac3b1b/apps/desktop/src/lib/imports/navicatImport.ts) · [Navicat 导出说明](https://help.navicat.com/hc/en-us/articles/218281627-How-to-get-the-connection-information-when-migrating-Navicat-to-new-computer)

## Windows 桌面验证

组合构建的 SHA-256 为 `377a04df07d272a089e98e4a57086002c7a763b097a4a3e04f3f812ae2a02723`，对应 `43948da` 的 `src-tauri` 源码树 `74b0ce4154c9109d750afcc7aec5e09e13fa586c`。验收使用独立的 RillSQL 数据目录和本仓 PostgreSQL 16 容器。SQLite 夹具包含能辨认文件的 `fixture_marker` 行。

| 操作 | 观测结果 |
| --- | --- |
| 从连接管理页选择真实脱敏 NCX，预览后取消 | 本地配置库仍为 0 条。[预览](preview.png) |
| 重新选取同一 NCX，取消勾选 MySQL SSH，只保留 PostgreSQL 和 SQLite | 成功 2、跳过 2、失败 0。MySQL HTTP 给出无法保留隧道的原因，MySQL SSH 标为未勾选。关闭报告，重开连接列表核对两个目标。[逐条结果](report.png) · [重开列表](reopened.png) |
| 再选完整 NCX，确认仍可导入的 MySQL SSH | 本地库 2 → 3。跳板持久化为 `bastion.example.invalid:2222`、用户 `fixture`、认证 `password`，两个已存密码标志为 0。 |
| 再次导入同一完整 NCX，按默认选择确认 | 成功 0、跳过 4、失败 0，本地库仍为 3 条。主动测试前，PostgreSQL 端的 RillSQL 会话数为 0。[重复导入](repeat-report.png) |
| 从原有 RillSQL 入口选择明文包、加密包和截断包 | 明文 3 → 4，错口令 4 → 4，正确口令 4 → 5，截断包 5 → 5 并提示文件错误。加密夹具使用合成口令按现有 RillSQL 格式生成。[错误口令](bundle-wrong.png) · [截断包](bundle-damaged.png) |
| 选取损坏的 NCX | 文件级 XML 错误，新增 0。[文件错误](file-error.png) |
| 重开连接列表，手动打开两个本地测试连接 | PostgreSQL 通过现有凭据提示补填后连接，容器 `pg_stat_activity` 返回 `rillsql\|postgres\|RillSQL`。SQLite 打开后显示已连接，配置库保存的文件经读取含 `fixture_marker = rillsql-import-fixture`。[PostgreSQL 实际连接](target-connected.png) · [SQLite 实际连接](sqlite-connected.png) |

MySQL 跳板与 HTTP 地址均为测试占位。该行以预览和持久化字段核验路由。Rust 回归另覆盖含测试凭据的 NCX、未知引擎、异常访问条件及部分勾选后的重复导入。[NCX 解析测试](../../../../src-tauri/src/adapters/profile_import/navicat.rs) · [桌面任务测试](../../../../src-tauri/src/app/shell/tests.rs)
