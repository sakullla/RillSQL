# 测试回执（T7）

运行环境：本机 macOS 共享工作区 `/Users/yanggeng/github/RillSQL`，基线为 `main` 上的 `eec04ec`
（T7 首轮提交）+ 复审补修（T7-A3）的未提交改动。首轮计数（124 / 1033）来自 `cb24998` + 首轮改动。GPUI headless 测试 + Rust 单元测试；所有 cargo 命令都带 `--locked`，
`src-tauri/Cargo.toml` / `src-tauri/Cargo.lock` 未改动（`git status` 里没有这两个文件）。

**证据归属**：下列运行都在「`cb24998` + 本轮未提交改动」的工作区上完成，不是某个提交上的干净复跑；
本任务不提交、不建分支，提交级基线需 Owner 在集成后补。

## 命令与结果

| # | 命令 | 结果 |
| --- | --- | --- |
| A | `cargo check --manifest-path src-tauri/Cargo.toml --lib --locked` | 退出码 0，0 error |
| B | `cargo check --manifest-path src-tauri/Cargo.toml --all-targets --locked` | 退出码 0，0 error |
| C | `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib app::views::grid` | ok：**128 passed / 0 failed / 2 ignored**（914 filtered） |
| D | `cargo test --manifest-path src-tauri/Cargo.toml --locked` | ok：**1037 passed / 0 failed / 7 ignored**；`main.rs` 0 passed；`src/bin/rillsql-package` 1 passed；doc-tests 0 |
| E | `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | 退出码 0，0 error；crate 级 warning 32 条，全部是既有项（`CELL_MENU`/`COLUMN_MENU` 只在测试里用、`cell_menu` 参数过多、`SheetDelegate` 可见性、`question_mark` 等），本轮新增代码没有新 warning 类别 |

## 本轮新增/替换的用例

| 用例 | 覆盖 |
| --- | --- |
| `pane_tests::modifier_click_adds_and_removes_single_cells` | Ctrl/Cmd 单击加一格（三格独立、`selection_count == 3`、块内空格留位 `1<TAB> / 2<TAB>乙`）、带表头 `copy_text`、再点同一格减掉（`selection_count == 2`、洞记在 `selection_holes`）——替换原先断言「加选 = 2x2 矩形」的错误用例 |
| `pane_tests::modifier_drag_adds_a_region_and_toggles_it_back_out` | Ctrl/Cmd 拖选加一块 1x2 区域；从已选区域再拖一遍把整块挖掉，区域在松开时收掉、洞一起清掉 |
| `pane_tests::real_mouse_events_reach_the_cell_gesture_handlers`（改写断言） | 真实鼠标事件下 Ctrl/Cmd 单击只加一格（`selection_count == 2`、`has_extra_regions()`、`1<TAB>` / `<TAB>乙`），普通单击与拖选矩形不变 |
| `pane_tests::copy_region_copies_the_value_for_one_cell_and_headers_for_many` | 单格 `copy-region` 给值本身；2x2 给含表头 TSV；`grid-copy-as-tsv` 同一份；`grid-copy-as-csv` 仍是 CSV；`copy-sql-value` 仍是 SQL 字面量 |
| `pane_tests::enter_edits_the_cell_and_ctrl_enter_opens_the_full_value` | Enter 进编辑并带出原值、Esc 取消、Ctrl/Cmd+Enter 打开完整值查看（与快捷键帮助的 `Enter / F2` 一致） |
| `pane_tests::double_clicking_a_column_divider_fits_that_column` | 分隔柄双击的等价入口 `TableState::fit_col` 量出宽度并写回会话（`> before + 100`），行号列返回 false |
| `workbench_tests::record_panel_stages_a_field_in_place_and_offers_batch_actions` | 记录面板字段就地暂存：Enter 写进暂存、Esc 丢弃且保留已有暂存、多格选区出现 `#grid-record-batch` |
| `workbench_tests::record_panel_explains_readonly_fields_instead_of_staging` | 只读结果点字段不进暂存：给会话只读原因并退化成查看完整值 |
| `workbench_tests::record_panel_reads_the_original_before_staging_a_long_text_field` | 长文本预览格点字段先按定位读原值：读不到不开编辑只解释；读到后输入框是原值，Enter 暂存的 UPDATE 里是原值而不是空串 |
| `workbench_tests::record_panel_never_opens_a_blank_editor_for_a_preview_cell` | 绕过入口直接摆出 `record_edit` 也不摆空输入框；Enter 不写空串 |
| `workbench_tests::record_panel_switching_rows_discards_the_in_place_edit` | 就地暂存开着时换行/关面板都丢弃这次输入，不写错行，重开不带上次文字 |
| `workbench_tests::record_panel_marks_preview_fields` | 预览字段有 `#grid-record-preview-{column}` 标记，普通值没有 |
| `shortcut_tests::every_grid_state_key_exists_in_the_catalog` | 7 个三态/动作键 + 记录面板与复制新键都在 `i18n::catalog_keys()` 里，中英都不回退成键名 |
| `workbench_tests::grid_flow_runs_at_three_widths_in_both_themes` | 1360/900/640 x 浅/深 x 紧凑/标准：加选 + 带表头复制 + 记录面板存在 + 内边距随密度 + 分隔柄自适应 |

## 相关既有用例（本轮未改，用于回归对照）

- `plain_click_replaces_an_existing_rectangle`、`mouse_and_keyboard_produce_the_same_rectangle_and_copies`、
  `dragging_across_cells_selects_the_rectangle_and_release_ends_it`、`row_header_selection_matches_dragging_across_the_row`：
  矩形选择、行头/列头与拖选语义没有回退。
- `every_cell_in_the_selection_rectangle_is_painted_selected`、`real_header_click_keeps_the_row_number_column_out_of_the_selection`：
  高亮与行号列规则没有回退（`cell_selected` 改成区域 + 洞后仍逐格验证）。
- `loading_outranks_emptiness_on_first_open`、`empty_grids_use_the_kit_states_and_their_own_next_step`、
  `a_first_read_failure_uses_the_kit_error_state_with_retry`、`a_failed_read_keeps_the_sheet_and_shows_one_error_entry`：三态与错误条。
- `failed_page_keeps_the_old_page_and_retries_the_same_offset`、`staged_edit_previews_keep_the_row_after_sorting`：失败与重排后的暂存保留。
- `hidden_columns_are_not_drawn_but_stay_in_copies`、`frozen_columns_stay_visible_while_scrolling_right`：隐藏列与冻结列。
- `set_value_on_selection_stages_one_batch_from_the_edit_bar`、`ctrl_v_pastes_tsv_only_from_the_grid_focus_and_respects_readonly`：批量原子拒绝。
- `app::shell::tests::explicit_null_and_copy_go_through_the_shared_shell_grid`：外壳经 `perform` 走同一条 `copy-region`（单格给值）。


## 复审补修（T7-A3）运行记录

补修内容见审计文档 13b 节（预览信封门控、换行/关闭丢弃暂存、预览标记）。命令与结果：

| # | 命令 | 结果 |
| --- | --- | --- |
| F | `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib app::views::grid` | ok：**128 passed / 0 failed / 2 ignored**（914 filtered） |
| G | `cargo test --manifest-path src-tauri/Cargo.toml --locked` | ok：**1037 passed / 0 failed / 7 ignored**；`src/bin/rillsql-package` 1 passed；doc-tests 0 |
| H | `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | 退出码 0，0 error；crate 级 warning 32 条，位置与首轮清单一致，没有新增类别 |

反向核对（确认回归用例真的咬住这两个缺陷，不是只跑一次通过）：

- 去掉 `grid-record-next` 里的 `discard_record_edit()`：`record_panel_switching_rows_discards_the_in_place_edit`
  在「换行丢掉这次就地暂存」处失败（`left: Some(1)`）。恢复后通过。
- 同时去掉 `record_field_press` 与 `begin_record_edit` 的原值判据：
  `record_panel_reads_the_original_before_staging_a_long_text_field` 与
  `record_panel_never_opens_a_blank_editor_for_a_preview_cell` 都在「不打开就地暂存」处失败。恢复后通过。

未解释的一次性现象：补修期间有一次全量 `cargo test --locked` 以
`panic in a destructor during cleanup`（tokio-rt-worker 线程）触发 SIGABRT 退出，日志里没有任何 `FAILED` 用例；
随后两次全量运行（含本次 #G）都是 1037 passed / 0 failed。该现象没有定位到根因，按未解决事实记录，
不计作通过证据，也不当作已排除。

## 未取得项 / 不计作通过

- **原生像素截图**（浅/深 x 紧凑/标准 x 1360/900/640）：本轮没有原生窗口与截屏通道授权，未生成 PNG，
  也没有用历史截图替代。按计划由 Delivery Owner 在最终集成 candidate 上采集。
- **真实数据库连接下的端到端手点走查**：本环境未启动 `docker/compose.yaml` 的实例，
  三档宽度与选择/复制/记录面板的证据来自 headless 窗口里的真实 `GridPane`（种入数据、真实键击与鼠标事件、真实绘制帧），
  不是连库后的手工走查。
- **宽松密度的原生走查**：`Metrics::relaxed()` 与外壳走同一条 `Metrics::publish` 路径，
  本轮在 headless 里验证了 token 与 delegate 生效值（行高 40 / 内边距 8），没有在原生窗口里用设置页切到宽松再截图。
- **原生双击分隔柄的像素级验证**：headless 里能拿到的是 `fit_col` 的等价入口与写回会话的列宽，
  真实鼠标双击命中分隔柄（2px 可抓取带）没有原生录屏证据。

## 复审补修（T7-A4）运行记录

补修内容见审计文档 13c 节（就地暂存绑行 id、失效收敛到一个钩子、finish 的原值兜底）。
工作区为 `main` 上的 `caef6de` + 本轮未提交改动（`git status` 里没有 `Cargo.toml` / `Cargo.lock`）。

| # | 命令 | 结果 |
| --- | --- | --- |
| I | `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib app::views::grid` | ok：**131 passed / 0 failed / 2 ignored**（914 filtered） |
| J | `cargo test --manifest-path src-tauri/Cargo.toml --locked` | ok：**1040 passed / 0 failed / 7 ignored**；`src/bin/rillsql-package` 1 passed；doc-tests 0 |
| K | `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | 退出码 0，0 error；crate 级 warning **32 条**，与 13b 的清单位置一致，本轮没有新增类别 |

本轮新增用例：`record_panel_mouse_row_change_discards_the_in_place_edit`、
`record_panel_sort_change_discards_the_in_place_edit`、
`record_panel_enter_never_writes_a_blank_over_an_unread_preview_value`；
既有记录面板用例的断言从"列号"改成"行 id + 列"（`Some(("1".to_owned(), 1))`）。

反向核对（每次只去掉一处，跑 `--lib app::views::grid::workbench_tests::record_panel`）：

- 去掉 `cell_press` 里的 `record_row_changed()`：`record_panel_mouse_row_change_discards_the_in_place_edit`
  失败（8 passed / 1 failed）。恢复后通过。
- 去掉 `finish_record_edit` 里的 `needs_full_of && full_text_of().is_none()` 判据：
  `record_panel_enter_never_writes_a_blank_over_an_unread_preview_value` 失败（8 passed / 1 failed）。恢复后通过。
- 去掉 `reload_after_condition_change` 顶部的 `record_row_changed()`：
  `record_panel_sort_change_discards_the_in_place_edit` 失败（8 passed / 1 failed）。恢复后通过。

本轮全量与分模块运行都没有复现 13b 记录的一次性 `panic in a destructor`
（tokio-rt-worker）SIGABRT；该现象仍按未解决事实保留，不作已排除。

未取得项与 13b 相同（原生像素截图、真实连库手点走查、宽松密度原生走查、原生双击分隔柄像素证据），
按计划交 Delivery。
