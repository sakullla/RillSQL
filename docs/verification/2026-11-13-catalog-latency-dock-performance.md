# v0.2.26：目录延迟、Dock 生命周期、查询与内存验证

日期：2026-11-13。基线为 v0.2.25（275fa53），保留该版本的对象树筛选、列详情、命令面板、网格编辑和筛选条。直接实现与验证，未创建研发 workflow。

## 行为变化

- 连接后在同一帧派发目录读取，避免一直显示“读取中”。右键编辑、测试、复制会切到可见的连接配置页，测试立即显示进行状态。
- MySQL、MariaDB、PostgreSQL、SQL Server 先读取库/模式概要，展开对象分类后再分页。分页使用实际 OFFSET/LIMIT；各连接逐条显示结果。元数据并发全局 4、每连接 2，单次读操作 30 秒超时；刷新合并、取消过期读取并拒绝旧响应。失败保留已有内容。
- SQL 补全复用已加载目录；列、外键、注释按 SQL 引用按需读取并防抖。对象树搜索明确为“筛选已加载对象”，不承诺全服务器搜索。
- 大表首屏与翻页不自动 COUNT(*)，点击行数可统计；翻页复用当前会话结构缓存，以服务端截断标记继续，避免旧计数截断新数据。统计与翻页并行不会互相覆盖状态。
- 唯一键定位从全体两两比较改为哈希计数，只复制键列。SQL 分块发布保留已有行的分配，移动临时缓冲，MySQL 连接 ID 直接来自握手，少一次往返。
- 连接探活并发受限，读取和握手具有忙碌保护；过期握手、重连、探活不能覆盖手动断开后的状态。写 SQL 不自动重放。
- 隐藏的导出面板不再每帧复制或保留完整结果。已加载数据导出在后台执行；文本格式缓冲写入，XLSX 使用 constant-memory worksheet 并直接保存临时文件。完成后释放快照，最终原子替换目标。
- macOS 注册 reopen：已有窗口激活，最后一个窗口关闭后再打开会重建窗口。全局动作只注册一次、弱引用当前窗口；连接服务在进程生命周期保留。

## 自动化与复现

标准命令：

```sh
cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked
node .github/scripts/release.mjs check v0.2.26
node --test .github/scripts/release.test.mjs
```

最终本机结果：863 项库测试、1 项打包测试通过，7 项需环境或性能基准的测试默认忽略；Clippy 退出码 0，保留项目既有提示，未增加全局 allow；12 项发布守卫测试通过。

专项测试覆盖真正的 GPUI 右键与按钮点击、连接后请求派发、关闭/重开与动作转发、慢连接公平性与超时、过期握手与断开竞态、忙碌探活、10 万行唯一键定位、百万行 SQLite 视图按需统计、后台导出与快照释放。恢复旧派发顺序后回归测试确实失败，恢复修复后通过。

### 高延迟协议验证

`scripts/verify-catalog-mysql.py` 使用 mysql-mimic 3.0.5，绑定 127.0.0.1:33067，虚拟目录含 100,000 张表。仅测试凭据 fixture / fixture。每个元数据查询分别延迟 500 ms 和 2 s；拒绝增长前缀查询及未预期的全量元数据读取。

```sh
python scripts/verify-catalog-mysql.py --delay 2
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib mysql_catalog_latency_fixture -- --ignored --nocapture
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib mysql_lazy_navigation_latency_fixture -- --ignored --nocapture
```

实测使用 mysql_async 真正的网络连接：概要不读取对象；偏移 0、100、99900 各返回 100 个对象；丢弃超时请求后下一次读取恢复。GPUI 测试覆盖连接、展开、下一页及连续 20 次刷新合并。2 s 延迟测试总耗时分别约 18 s、19 s，不等待全部 10 万个对象。取消时 fixture 可记录 BrokenPipe，属于主动取消连接。

这是 MySQL 协议模拟及本机 SQLite 验证；未连接用户业务库，未实测远端 PostgreSQL/SQL Server 集群或真实广域网丢包环境。

## 同机性能对比

macOS ARM64，同一 Rust 工具链与 debug 测试 profile，旧版 worktree 加入完全相同的两个 ignored benchmark。构建结束后串行测量，无并发编译。时间为 benchmark 内测量区间；RSS 来自 `/usr/bin/time -l` 的独立测试进程峰值。单次测量，有调度/文件系统缓存噪声，不能外推为 release 性能、整窗 RSS 或服务端 SQL 执行加速。

| 场景 | v0.2.25 | v0.2.26 |
| --- | ---: | ---: |
| CSV 导出，100,000 行 × 4 个文本列 | 204,436 µs | 83,415 µs |
| 上述进程最大 RSS | 353,222,656 B（336.9 MiB） | 96,288,768 B（91.8 MiB） |
| 结果定位，10,000 行 × 8 列 | 3,122,899 µs | 22,719 µs |
| 上述进程最大 RSS | 71,712,768 B（68.4 MiB） | 53,198,848 B（50.7 MiB） |

CSV 两版均为 55,655,578 字节，FNV 校验和 `247c011bdc12e508`。定位输入包含 17,920,000 字节非键正文；新版不复制这些正文。定位耗时下降约 99.3%，CSV 测试峰值 RSS 下降约 72.7%。结果发布通常只有两个批次，本次未把该路径误称为平方复杂度；真正消除平方比较的是唯一键定位。

复测方法：`cargo test --locked --manifest-path src-tauri/Cargo.toml --lib --no-run` 后，用输出的确切测试可执行文件分别运行 `benchmark_loaded_export_memory --ignored --nocapture --test-threads=1`、`benchmark_result_location --ignored --nocapture --test-threads=1`，前置 `/usr/bin/time -l`。

## 原生启动证据范围

使用独立 bundle ID `app.rillsql.dockverify`、临时 app 和 debug 专用绝对路径资料目录。通过 LaunchServices 的 `open -a` 从 `/` 启动和再次打开；检查 NSRunningApplication 的激活状态与 CGWindowList 的主窗口。不会触及用户已安装应用及连接资料。

最终 v0.2.26 debug 二进制已重建并进行上述冷启动及隐藏后再次打开：同一 PID 89736、同一 1280 × 820 主窗口，均 `active=true`、`finishedLaunching=true`、`onscreen=true`。原始证据在 [cold](catalog-dock-2026-11-13/native-cold.json)、[warm](catalog-dock-2026-11-13/native-warm.json)、[性能记录](catalog-dock-2026-11-13/performance.json)。

物理关闭按钮自动化和截图受当前 Accessibility/屏幕权限限制；关闭后重建及动作路由由 GPUI 生命周期测试验证。LaunchServices 冷启动/再次激活不等同于实际点击 Dock 或从下载 DMG 安装。Windows/Linux 原生 GUI 启动未在本机验证，跨平台 CI 和打包结果也不能替代这些证据。

## CI 后续修正（2026-11-14）

首轮 Linux 测试和 Clippy 通过。Windows 为 862 通过、1 失败：`exchange_loaded_export_omits_uncommitted_cells` 的 GPUI scheduler 报告真实 Tokio 线程唤醒未被允许。导出改为后台执行后，该旧测试缺少 `allow_parking()`；已补齐测试配置，并让两处导出测试同时等待文件和 UI 任务终态，避免只见文件就提前退出。修改仅涉及测试，不改变已验证的生产二进制。tag 尚未推送，修正提交继续使用未发布的 0.2.26。
