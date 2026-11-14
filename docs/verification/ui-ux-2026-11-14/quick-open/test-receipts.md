# 测试与命令回执（T5）

全部命令在 `/Users/yanggeng/github/RillSQL` 工作区（HEAD `d94f66f` + 本任务改动）上串行执行，未并发第二个 cargo。

## 1. 正式命令

| 命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked` | 通过。lib: **1016 passed / 0 failed / 7 ignored**（26.48s，最终复跑）；`rillsql-package` 1 passed；Doc-tests 0。 |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib app::shell` | 通过。**187 passed / 0 failed / 1 ignored**（6.72s）。 |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | exit 0。`app/shell/` 里只剩 3 条既有 warning（`chrome.rs:4039` 参数过多、`menus.rs:598` let 绑定、`model.rs:1655` 可用 `?`）；本轮新增代码 0 条（新增用例里曾出现的 `useless clone` 已改掉）。 |
| `git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock` | exit 0：依赖与锁文件未变。 |

正式验证按计划由 Delivery Owner 在集成后的 candidate 上重跑；本回执只证明本任务范围内通过。

## 2. 焦点用例

`cargo test --locked --lib app::shell::tests::palette` → **8 passed**（1 个既有：`palette_keyboard_reaches_compare_exchange_and_help_and_restores_focus`）：

| 用例 | 覆盖 |
| --- | --- |
| `palette_searches_connections_databases_tables_views_saved_queries_and_commands` | 六类来源都在；每类按名字与上下文都能命中；六种类型标注文案互不重复；`detail` 依次是 `PostgreSQL · app` / `销售库` / `shop.public · 销售库` / `销售库 · shop` |
| `palette_command_shortcuts_match_the_shortcut_help_and_menu_source` | 逐条比 `entry.shortcut` 与 `COMMANDS.chord`→`SHORTCUTS.chord`；非命令条目没有快捷键；`>` 只留命令（中文界面按中文名命中，英文界面 `> new` 也能命中，`>` 后空格不参与匹配） |
| `palette_opens_data_loads_saved_and_focuses_the_tree` | 表行打开数据页（`executions == 0`）；保存查询只载入草稿（带原连接 / 库、`last_executed_sql` 为空、`executions == 0`、`loads == 1`、只多一个工作项、焦点到编辑器）；库行聚焦树并展开连接；连接行落在连接行；库里没有该节点 / 连接已被删除各有原因文案 |
| `palette_keyboard_walks_every_type_and_enter_runs_the_selected_row` | 打开面板后 ↑↓ / Home / End 在混合结果里走且循环；光标落到保存查询行回车 → 关闭面板、载入草稿、不执行 |
| `palette_targets_connect_first_when_the_connection_is_not_open` | 未连接时表行先建连并排 `OpenData` 意图（不直接开数据页）；连上后重放出排队时的 spec 且只重放一次；连接 / 库行排队 `replay`；在途连接复用；保存查询不建连 |
| `palette_never_invents_unloaded_objects_and_separates_no_match_from_failure` | 没读过的对象不出现；`ui.palette.noMatch` 与 `ui.palette.error` 是两条文案；失败时带原因；清空搜索词恢复 |
| `palette_drops_stale_rows_when_the_connection_goes_away_and_restores_them_after_reconnect` | 断开后表 / 视图 / 库一起消失、连接与保存查询仍在；重连后同一份索引回来；目录刷新失败时对象与库一起清空并留原因 |

`cargo test --locked --lib app::shell::tests::quick_open` → **3 passed**：

| 用例 | 覆盖 |
| --- | --- |
| `quick_open_lists_loaded_tables_and_views_and_opens_the_matching_target`（既有，本轮加类型断言） | 快速打开只列表 / 视图；表标“表”、视图标“视图”；Redis 键不出现；断开 / 删除后不冒充可打开 |
| `quick_open_lists_the_loaded_tree_and_opens_without_running_sql`（既有，本轮加类型断言） | 真实 SQLite 会话（表 `item` + 视图 `item_view`）下按 `open-table:` 打开数据页且不执行 SQL |
| `quick_open_renders_context_and_type_for_every_result_kind`（新增，headless） | 640×800 下 `pal-kind-*` 的无障碍名称等于对应类型的文案键（表 / 视图 / 保存的查询），`pal-detail-*` 真有占位尺寸，面板矩形不越界且宽度 ≤ 620 |

## 3. 证据缺口

- 没有原生截图与原生键盘录屏：本环境无原生窗口通道，见 `README.md` 的「证据类型」。
- `quick_open_renders_*` 的尺寸断言是 headless 布局证据，不是像素证据；对象索引来自真实 SQLite 会话、保存查询来自真实历史库，这一点与纯模型断言不同。
