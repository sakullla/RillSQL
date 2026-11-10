# GPUI 工作台导航补全与网格体验深化 · T7 验收报告

- 日期：2026-11-10
- 任务：T7（响应式、主题密度、语言与焦点一致性验收），依赖 T3、T6
- 基线：`0761a66`
- 结论：真实原生窗口已启动并完成代表性截图；深色/紧凑/英文、浅色/标准/中文均实际切换验证。大表查询“永远执行中、无数据”与网格工具栏语言不同步两个真实缺陷已复现、修复并重新验证。

## 1. 自动验证

| 命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked -- --test-threads=1` | **677 passed / 0 failed / 0 ignored**（139.06s）；`rillsql-package` 1 passed；doc-tests 0 |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | exit 0；仅 `src/bin/rillsql-package.rs` 两条既有 `dead_code` 警告 |
| `cargo build --manifest-path src-tauri/Cargo.toml --bin rillsql --locked` | 成功 |

覆盖：`display_matrix_keeps_primary_controls_visible_and_unoverlapped`（1360/900/640×900）、`locale_theme_and_density_refresh_existing_inputs_tree_and_palette`、`background_reads_keep_navigation_usable_and_latest_context_wins`、网格 Esc 焦点归还、查询流式回归与全量既有回归。

## 2. 原生窗口证据

窗口在 150% DPI 下调整；截图包含窗口边框，客户区逻辑尺寸约 1345×885、885×885、640×885，分别对应目标 1360/900/640 三档布局区间。侧栏在 640×900 采用覆盖式，收起后主操作和结果可见。

| 证据 | 内容 |
| --- | --- |
| `gpui-workbench/native-1360x900.png` | 1360×900、浅色、标准密度、中文；查询结果可见 |
| `gpui-workbench/native-900x900.png` | 900×900、浅色、标准密度、中文 |
| `gpui-workbench/native-640x900.png` | 640×900、浅色、标准密度、中文；侧栏覆盖展开 |
| `gpui-workbench/native-1360x900-dark-compact-en.png` | 1360×900、深色、紧凑、英文；100 行结果与 Load more 可见，工具栏已英文 |
| `gpui-workbench/native-900x900-dark-compact-en.png` | 900×900、深色、紧凑、英文 |
| `gpui-workbench/native-640x900-dark-compact-en.png` | 640×900、深色、紧凑、英文；侧栏覆盖展开 |
| `gpui-workbench/native-640x900-dark-compact-en-closed.png` | 640×900 收起侧栏；Run、结果和 Load more 可达 |
| `gpui-workbench/native-settings.png` | 浅色、标准、中文设置页；主题/密度/语言控件 |
| `gpui-workbench/native-settings-dark-compact-en.png` | 深色、紧凑、英文设置页实录 |

已观察：
- 各尺寸下顶栏、活动栏、工作区、状态栏不重叠；640×900 侧栏为覆盖层，不挤压主工作区。
- 主题、密度、语言切换后，已存在的树、设置页、查询工具栏和结果网格立即更新；网格工具栏原先残留中文，修复后为完整英文。
- 结果区在查询执行时显示首批 100 行和“Load more”，不再一直空白或永久“正在执行”。

## 3. 原生缺陷修复

1. **大表查询中间结果不可见**：`exec::reveal_rows` 分块发布后等待继续读取，但 `QueryPane::spawn_run` 只消费终态，导致 `select * from thread_items`（32764 行）永久显示“正在执行”。改为单一有序 mpsc 通道消费每个 running/terminal 快照；运行态保持 `running=true`，终态才标记 executed。原生 2.2 秒内已显示 100 行。
2. **网格语言不同步**：`GridPane` 是独立实体，父级 QueryPane 换语言时只更新自身，网格仍为中文。已在 QueryPane 的全部分同步入口下发 locale、在 Workbench 表数据同步中下发 locale，并在网格实体 notify。结构/管理页即使 `applied_tool` 缓存命中也会收到 locale；对比、导入导出和关系图在每次同步时下发 locale。原生深色紧凑英文截图确认网格工具栏为英文。

## 4. 未覆盖边界

1. 未穷举完整笛卡尔矩阵；原生实测覆盖浅色/标准/中文与深色/紧凑/英文在三种宽度上的代表性组合，其余组合由 GPUI 测试窗口断言覆盖。
2. 未验证真实 DPI 缩放链、IME 候选框位置、原生 tooltip/menu 层级与屏幕阅读器；640 宽使用侧栏覆盖而非并排布局。
3. 原生查询验证使用本机 SQLite 文件，未连接 PostgreSQL/MySQL/MariaDB/SQL Server；其他引擎仍依赖测试 fixture。
4. 本报告截图来自实际 Windows 原生窗口（Pillow `ImageGrab`），不是离屏测试渲染；截图文件已加入 `.gitignore` 白名单。

## 5. 修改路径

- `src-tauri/src/app/shell/chrome.rs`
- `src-tauri/src/app/shell/layout.rs`
- `src-tauri/src/app/shell/model.rs`
- `src-tauri/src/app/shell/tests.rs`
- `src-tauri/src/app/shell/view.rs`
- `src-tauri/src/app/views/grid/mod.rs`
- `src-tauri/src/app/views/query/mod.rs`
- `src-tauri/src/query/desk.rs`
- `.gitignore`
- `docs/verification/gpui-workbench/*.png`
- 本报告

## 6. 复现

```bash
cargo test --manifest-path src-tauri/Cargo.toml --locked -- --test-threads=1
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked
cargo build --manifest-path src-tauri/Cargo.toml --bin rillsql --locked
```
