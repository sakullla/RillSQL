# 测试与命令回执（T8）

全部命令在仓库工作区（HEAD `0ff974d` + 本任务改动）上于 2026-11-14 实际执行。
本文件只证明**本任务范围内**通过；三条正式命令由 Delivery Owner 在集成后的同一 candidate 上重跑。

## 1. 正式命令

| 命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked` | 通过：**1051 passed / 0 failed / 7 ignored**（27.25s）；另有 `rillsql-package` 1 passed、`main` 与 doc-tests 各 0 |
| `cargo test ... --lib -- shell::` | 通过：**193 passed / 0 failed / 1 ignored** |
| `cargo test ... --lib -- views::` | 通过：**296 passed / 0 failed / 2 ignored** |
| `cargo test ... --lib -- app::views::history app::views::settings` | 通过：**12 passed / 0 failed**（两页模型层各 6 条，本任务新增 5 条） |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | 通过（exit 0）：29 条 warning，其中仅 2 条落在本任务改过的文件，且都是既有代码（`chrome.rs:4287` `context_menu_box` 参数过多、`model.rs:1671` 可用 `?`）；本任务新增代码 0 warning |
| `git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock` | 通过（exit 0），Cargo 依赖未变 |

ignored 的 7 条全部是既有需要外部 fixture/平台的用例（sqlite fixture 之外的远端数据库等），本任务没有新增 ignored 用例。
`style_literals_follow_design_tokens` 与 `style_scan_rejects_reverted_literals` 都在上面这次全量运行里通过。

## 2. 本任务新增用例

### 2.1 视图层（`app::views::history::tests`、`app::views::settings::tests`）

| 用例 | 覆盖 | 证据类型 |
| --- | --- | --- |
| `loading_failure_empty_and_no_match_stay_distinct_for_both_sections` | 读取中 ≠ 无数据；失败结束读取并优先于无命中；两个分区共用同一判断；刷新期间已有行时不换骨架；`apply_list` 复位读取标记 | 源码级 |
| `keyboard_selection_walks_filtered_rows_and_maps_to_the_same_commands` | ↑↓/Home/End 只在筛选后的可见行里移动、到边界不再"假装移动"、无行时不凭空选中、无命中时不保留不可见选中；回车载入命令与删除命令按分区映射 | 源码级 |
| `sections_have_stable_commands_and_labels` | 三个分区命令 id 稳定可反解、标签键互不相同、未知 id 不解析 | 源码级 |
| `save_state_only_reports_copy_where_the_user_needs_it` | `Idle` 不打扰、`Saving/Saved/Failed` 各有文案键、只有失败带底层原因 | 源码级 |
| `only_commands_that_change_settings_use_the_feedback_path` | 会改本机设置的 12 个代表样本命令走带反馈的写盘；分区导航、设置页本身、历史预览按钮、新建查询不走（反向也断言） | 源码级 |

### 2.2 外壳与渲染（`app::shell::tests`）

| 用例 | 覆盖 | 证据类型 |
| --- | --- | --- |
| `history_states_are_distinct_and_keep_the_state_action_inset` | 读取中只有骨架、失败只有错误块 + `history-retry`、无数据与无命中用不同语义块；主操作相对说明左移 `38`（`STATE_ACTION_INSET`）、说明宽 ≤ `360`（`STATE_DETAIL_MAX_W`） | headless 布局级 |
| `history_keyboard_walks_rows_and_enter_only_loads` | 真实按键：↑↓/End/Home 改选中；Enter 只让 `loads + 1`、`executions` 不变、草稿是该条 SQL；Delete 先开确认层且不累计 `shell.marked_delete`；键位绑定入口（`perform("mark-delete")`）落到同一条命令 | headless 交互级 |
| `settings_sections_switch_and_nav_stacks_when_narrow` | 点击分区导航切换右侧表单、一次只渲染一个分区；1360 导航 `w = 180` 且在表单左侧；640 导航折到表单上方，两者右边界都在窗口内 | headless 布局级 |
| `failed_settings_write_is_visible_then_retry_persists` | 状态文件路径被目录占住 → `settings_save == Failed`、设置页出现 `settings-retry-save`、状态栏出现 `status-settings-save`，改动仍然立即生效；路径恢复后点重试 → `Saved`，`services.load_state()` 读到的就是当前字号；成功后两处提示同时消失 | headless + 真实文件写盘 |
| `history_and_settings_geometry_holds_in_both_themes_densities_and_three_widths` | 浅/深色 × 紧凑/标准 × 1360/900/640：历史行高 67.5/71.5（差 4 = 2×`row_pad_y` 差）、设置外观卡片 180/204（差 24 = 3 行 × 8）、主题不改几何、导航与卡片不越出窗口 | headless 布局级 |
| `settings_and_history_breakpoints_follow_content_width` | 断点按内容宽度判断：1360/1074、900/848 → 并排；760/708、640/588 → 堆叠；同一个 900 窗口把侧栏停靠后内容变窄，两页同时改走堆叠 | 源码 + headless 布局级 |

## 3. 回归相关的既有用例（本任务范围内全部通过）

`history_search_filter_preview_copy_delete_and_clear_are_reversible`（载入不执行、筛选、复制、清空确认）、
`history_preview_uses_stacked_layout_when_content_is_narrow`（<820 堆叠）、
`history_and_settings_pages_render_and_apply_their_controls`（页面控件点选，本次改为先切到"数据读取"分区再点读取上限）、
`every_public_setting_moves_both_ways_applies_immediately_and_persists`（14 个设置项双向、即时生效、重启读回）、
`public_settings_reach_the_query_pane_without_a_restart`、
`copy_fits_every_width_density_theme_and_locale`（本任务新增的 14 个文案键在 1360/1024/900/760/640 × 侧栏开合 × 双主题 × 双语言 × 三密度下都放得下）、
`dictionaries_match_the_existing_shell_keys`、`both_locales_translate_every_catalog_key`。

## 4. 未执行 / 未取得

- **未运行原生应用**：本环境没有图形会话，`cargo run --bin rillsql` 无法打开窗口，因此没有原生启动、Tab 走查、
  IME、菜单与截图证据。R6 的四流程端到端由 Delivery Owner 在集成 candidate 上补。
- **未做重启验证**：重启持久化只在 headless 里以"真实写盘 + `load_state()` 读回"证明（见 2.2 第 4 条），
  没有第二进程重启的原生证据。
- 本任务未改动 `src-tauri/Cargo.toml` / `Cargo.lock`，也未新增 ignored 用例。

## 5. A2 修复回执（2026-11-14）

基线：HEAD `441ef6f`（T8-A1）+ 本次改动。两条问题都只在源码/headless 级验证，
原生走查与截图仍然不可得（同第 4 节）。

| 命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked app::shell` | 通过：**195 passed / 0 failed / 1 ignored** |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked app::views` | 通过：**296 passed / 0 failed / 2 ignored** |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked` | 通过：**1053 passed / 0 failed / 7 ignored**（28.58s）；另有 `rillsql-package` 1 passed、`main` 与 doc-tests 各 0 |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | 通过（exit 0）：32 条 warning，全部是既有代码；本次改动的行 0 warning |
| `git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock` | 通过（exit 0），依赖未变 |

### 5.1 新增用例

| 用例 | 覆盖 | 证据类型 |
| --- | --- | --- |
| `history_confirm_sheets_own_the_keyboard` | 删除确认开着时连按 ↓ / Enter / Delete：`selected_saved`、`delete_target`、`shell.page`、`pending_len` 全不变（不换选中、不载入换页、不排队删除）；取消后 ↓ 恢复；清空历史开着时 ↓ / Delete 同样不生效、不排队 `ClearHistory`/`DeleteRun` | headless 交互级（真实按键） |
| `settings_save_completes_when_an_unrelated_command_lands_first` | 设置改动进入 `Saving` 后同一片段再跑一条无关命令（切换页面，走静默写盘并推进共享代次）：设置状态仍回到 `Saved`，`services.load_state()` 读到的就是当前设置 | headless + 真实文件写盘 |
| `history_search_filter_preview_copy_delete_and_clear_are_reversible`（既有，追加断言） | 打开删除确认后把选中换成另一条：`delete_target()` 仍是打开确认时那条，`job_request(DeleteSaved)` 的 `saved_id` 也是它——异步作业不跟着选中漂移 | 源码级 |

### 5.2 因果验证（先证失败，再证通过）

| 修复 | 临时去掉改动后的结果 | 恢复后 |
| --- | --- | --- |
| 确认层让路（`overlay_key` 分支 + `history_key` 早退 + `mark-delete` 分流） | `history_confirm_sheets_own_the_keyboard` FAILED：`panicked at tests.rs:3787: assertion left == right failed: 方向键不改选中` | 通过 |
| 设置写盘状态不再被代次卡住 | `settings_save_completes_when_an_unrelated_command_lands_first` FAILED：30s 超时未回到 `Saved`（`pump_until` 返回 false） | 通过 |

### 5.3 本轮新增/改动的路径

`src-tauri/src/app/shell/chrome.rs`（历史键位分支前提）、`src-tauri/src/app/shell/model.rs`（`history_key` 早退、删除目标钉住与清理）、
`src-tauri/src/app/shell/view.rs`（`mark-delete` 分流、设置写盘结果判定）、`src-tauri/src/app/shell/jobs.rs`（`saved_id` 取钉住的目标）、
`src-tauri/src/app/views/history/mod.rs`（`confirm_open`/`delete_target`）、`src-tauri/src/app/shell/tests.rs`（两条新用例 + 一条既有用例追加断言）、
本目录 3 个文档。i18n 未新增文案键。

