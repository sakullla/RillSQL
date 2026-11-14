# 测试与命令回执（T4）

全部命令在本任务工作区（HEAD `da15f59` + 本任务改动）上串行执行，没有并发第二个 cargo。

## 1. 正式命令

| 命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked` | 通过。lib: 1010 个用例 **1003 passed / 0 failed / 7 ignored**（26.11s）；`rillsql-package` 1 passed。 |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | exit 0；32 条 warning 全部是既有代码（`diagram`、`grid`、`redis`、`ddl`、`layout::painted_frame`、`menus::close_submenu`、`model::menu_key` 等）。本任务新增文件（`explorer.rs`、`groups.rs`、`connections/{store,service}`、`shell/{chrome,model,jobs}` 的改动行）0 条。 |
| `git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock` | exit 0：依赖未变。 |

按计划这三条由 Delivery Owner 在集成后的同一 candidate 上重跑；本回执只证明本任务范围内通过。

## 2. 焦点用例（本轮新增）

`cargo test --locked -- app::shell::explorer::` → **8 passed**：

| 用例 | 覆盖 |
| --- | --- |
| `window_covers_viewport_only_and_pads_the_rest` | 1000 行只看视口 10 行，上下空白等于其余高度 |
| `element_count_does_not_grow_with_total_rows` | 20/200/5000/100000 行创建元素数一致且 ≤ 视口+overscan |
| `mixed_heights_keep_scroll_coordinates_consistent` | 连接行 32 与树行 30 混排时窗口起点与上下空白 |
| `unknown_viewport_and_scroll_never_render_everything` | 视口为 0/NaN 时按 480 兜底；滚过末尾按最后一段算 |
| `empty_list_has_no_window` | 空序列 0..0，无空白 |
| `scroll_to_row_offsets_are_clamped_to_content` | 定位偏移被内容夹住 |
| `focus_rows_skip_notes_and_errors_and_keep_stable_ids` | 说明/错误/无命中行不参与键盘；`group:`/`conn:` 前缀 |
| `heights_follow_density_metrics` | 行高随密度取自 `Metrics` |

`cargo test --locked -- app::shell::tests::{explorer*,group*,hundreds*,searching*,filtering*,empty_databases*,object_browser*,dragging*,groups_and_collapse*,unknown_group*}` → **15 passed**（含既有 `explorer_tree_reads_real_levels_and_loads_columns_on_demand`）：

- `explorer_flat_rows_group_connections_and_keep_empty_groups_visible`
- `explorer_ids_are_stable_and_dense_under_hundreds_of_connections`
- `explorer_keyboard_walks_groups_connections_and_tree_rows`
- `explorer_rows_keep_long_names_status_chevrons_and_object_actions`
- `group_commands_share_one_pending_job_and_reject_empty_or_missing_targets`
- `group_row_menu_and_connection_drag_share_the_same_move_command`
- `dragging_a_connection_onto_a_group_row_queues_the_same_assign_command`
- `groups_and_collapse_state_survive_a_restart_through_the_job_path`
- `hundreds_of_connections_render_only_the_visible_explorer_rows`
- `searching_reads_only_loaded_nodes_and_clearing_restores_the_tree`
- `filtering_to_nothing_shows_one_clear_no_match_row`
- `empty_databases_and_connection_errors_have_their_own_explorer_rows`
- `object_browser_geometry_holds_at_1360_900_and_640`
- `unknown_group_ids_read_as_ungrouped_and_group_metadata_stays_out_of_connections`
- `explorer_tree_reads_real_levels_and_loads_columns_on_demand`（既有）

`cargo test --locked -- connections::groups connections::tests::{groups_persist,deleting_a_group,group_names,unknown_group_ids}` → **8 passed**（4 个 `groups.rs` 纯函数用例 + 4 个真实配置库用例）。

## 3. 回归

- `cargo test --locked -- app::shell`：**173 passed / 1 ignored**（含 T3 的模态、启动台、按需连接与对象树既有用例）。
- 全量 lib 1003 passed：查询、网格、结构、对比、导出、Mongo、Redis、管理页用例未受影响。
- 改动过 `catalog::hierarchy::visible_under`（筛选时只走已加载节点并保留祖先）：`object_search_keeps_the_tree_shape_and_clearing_restores_the_tree`、`explorer_tree_reads_real_levels_and_loads_columns_on_demand`、`queued_group_retry_dispatches_a_read_and_resolves_loading` 均通过。

## 4. ignored 用例

7 个 ignored 全部是既有的远端 fixture 用例（MySQL/PostgreSQL/Redis/Mongo 等），本任务没有新增 ignored；它们**不计作通过**。

## 5. 评审修复 A2（非当前库按库懒加载）复跑

同一工作区、同一 HEAD（`2086aac`）+ 本轮改动，串行执行，没有并发第二个 cargo。

| 命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib catalog::` | **60 passed / 0 failed / 1 ignored**（954 filtered）。新增 4 个：`a_non_current_database_loads_layer_by_layer_with_a_loading_state`、`a_failed_database_read_keeps_the_reason_on_that_database_only`、`sqlserver_sql_is_scoped_with_three_part_names`、`a_database_scope_read_needs_a_database_on_a_layered_engine` |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib app::shell` | **179 passed / 0 failed / 1 ignored**（835 filtered）。新增 1 个：`a_database_outside_the_current_session_reads_on_expand_and_keeps_its_own_error` |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked` | 通过。lib **1008 passed / 0 failed / 7 ignored**（26.11s）；`rillsql-package` 1 passed |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | exit 0；32 条 warning，与修复前同数，全部既有代码；本轮的 `catalog/{read,detail,hierarchy}.rs`、`app/shell/{nav,model,view,tests}.rs` 0 条 |
| `git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock` | exit 0：依赖未变 |

- 改过的既有用例：`catalog::tests::refresh_keeps_the_loaded_window_and_a_failed_list_is_not_empty` —— 非当前库从“带错误”改为“`error: None` + 空 schemas”，断言随实现修正。
- ignored 仍是既有的 7 个远端 fixture 用例，本轮没有新增 ignored，**不计作通过**。
- 详细路径与仍未解决的相邻边界见 `database-lazy-loading.md`。
