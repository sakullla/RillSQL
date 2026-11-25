# UX 第三轮验证报告（2026-11-24 ~ 11-25）

本轮需求 R1–R16 的验证证据。所有夹具只用通用名称（`shop`、`shop_search_a`、`orders`、`order_lines`、`wide_orders`、`demo-uat` 等）。

## 结论

- **Rust 测试**：`cargo test --manifest-path src-tauri/Cargo.toml --locked` 全部通过（T13 后 2077 个通过，0 个失败）；`cargo clippy --all-targets --locked` 无警告。
- **原生截图（2026-11-25 补拍）**：在解锁的 macOS 会话里，用 debug 构建和隔离资料目录（`RILLSQL_DEBUG_DATA_DIR`）对 docker 通用夹具逐页操作并截图，共 33 张，见下表。连接只用临时账号 `demo_user`（拍完即删除），密码只在本次运行内记住，没有写入保险库或系统钥匙串。
- **R15 逐页巡检**：覆盖侧栏、标签栏、表数据、查询、结构、执行计划、对话框、设置。巡检发现的 5 处细节已由 T12 修复，并在修复后补拍（07b、07c、07e、08b、11c、12c）。
- **未实测**：R7“升级后钥匙串最多授权一次”需要用户本人在正式安装包和真实钥匙串上点击系统授权，本轮没有实测；对应行为由 Rust 测试覆盖。

## 原生截图

| 文件 | 需求 | 场景 |
|---|---|---|
| 01-grid-edit-cell-rows-below-visible | R1 | 编辑 `shop.wide_orders` 单元格，下方各行不被遮挡，快捷键提示在工具栏 |
| 01b-grid-new-row-tab-to-col25 | R2 | 新增行带“新”标记；Tab 到第 25 列时当前格完整可见 |
| 02-grid-column-panel | R3 | 工具栏唯一的“列”入口，面板可搜索、显隐、冻结、排序 |
| 02b-grid-column-layout-kept-after-reopen | R3 | 隐藏 2 列、冻结 1 列后重开表，布局保持（`column-layouts.json`） |
| 04-sidebar-search-map-two-connections | R4 | 两个已连接的连接同时搜 `map`，每个连接 51 个命中，按库分组 |
| 04b-sidebar-search-expanded-db-stays-open | R4 | 手动展开的命中库（25 个）保持展开 |
| 05-tree-context-menu-copy-shortcut | R5 | 右键菜单“复制完整名称 ⌘⌥C” |
| 06-sidebar-drag-insert-line | R6 | 拖动连接时目标行上沿的插入线 |
| 06b-sidebar-order-kept-after-restart | R6 | 重启应用后连接顺序保持 |
| 07-settings-security-card | R7 | 设置 → 本地数据 → 安全：“始终允许”说明 |
| 07b-vault-file-access-banner | R7 | 保险库文件不可读时的提示条，按钮为“重试” |
| 07c-settings-security-card-with-problem | R7 | 安全卡片显示保险库问题和“重试” |
| 07e-password-prompt-vault-wording | R7 | 密码输入框“记住密码（加密保存在本机保险库）” |
| 08-query-ddl-summary-card | R8 | 建表成功的摘要卡片，完整 SQL 默认折叠；失败语句行内标记 |
| 08b-tab-overflow-list | R8 | 标签列表：图标、环境、路径、关闭按钮、当前项高亮 |
| 08c-index-editor-two-pane | R8/R13 | 索引编辑左右两栏，已选列有序号和升降序 |
| 09-query-error-card-open | R9 | 错误卡片：错误码、可选中文字、清除标记、复制错误 |
| 09b-query-error-card-closed-by-esc | R9 | 按 Esc 后卡片关闭，SQL 不被遮挡 |
| 10-query-alias-candidates | R10 | 候选中列出本语句别名及其指向的表（截图时输入法残留了一个全角句号，不影响候选） |
| 10-query-alias-column-completion | R10 | `w.` 补全 `wide_orders` 的列 |
| 10b-query-alias-column-completion-current-db | R10 | `ol.` 补全 `order_lines` 的列（按需加载） |
| 10c-settings-auto-alias-toggle | R10 | 设置中“补全表名时自动添加别名”开关 |
| 11-plan-canvas-three-table-join | R11 | 三表 JOIN 画布：连接节点、连线、表名加别名、全表扫描标红 |
| 11b-plan-canvas-selected-node-detail | R11 | 选中 `customers c`，边框加外发光，文字清晰，右侧详情同步 |
| 11c-plan-canvas-refit-after-maximize | R11 | 结果区最大化后画布自动重新适配（84%） |
| 12-query-risk-modal-call | R12/R14 | CALL 的模态确认 |
| 12b-query-risk-modal-20-statements | R12 | 20 条危险语句：中间滚动、底部按钮固定 |
| 12c-query-risk-cancelled-note | R12 | 取消后“已取消：未执行任何语句”单独显示在结果区状态栏 |
| 13-structure-pk-strip | R13 | 主键条 `PRIMARY KEY (order_id, line_no)`，PK1/PK2 标记 |
| 13b-structure-pk-extended-pending | R13 | 把 `sku` 加入主键，待应用并提示将重写整张表 |
| 13c-structure-pk-change-preview | R13 | 预览：SQLite 12 步重建与危险提示，需再次确认 |
| 14-query-call-two-result-sets | R14 | `CALL shop.report_two_sets()` 产生 CALL·1、CALL·2 两页 |
| 14b-query-call-messages-warning | R14 | 消息页显示两个结果和 Warning 1642 |

## 已知限制（原生巡检记录）

- MySQL TREE 计划的节点名（如 “Nested loop inner join”“Single-row index lookup”）原样显示英文。
- 以下问题在截图后由 T13 修复，现有截图拍摄于修复前：跨库限定表（如 `shop_a.orders o`）的别名列补全改为按需加载；结果区较小时画布自动适配不低于 70%，可平移查看。
- 08-query-ddl-summary-card 与 09b-query-error-card-closed-by-esc 是同一帧：DDL 摘要卡片折叠、按 Esc 后错误卡片已关闭，两种状态都在这张图里。
- 方案修订 6 之后，设置 → 安全卡片新增的“还有 N 个旧密码未迁入 / 现在全部迁入 / 不再迁移”没有原生截图，由 Rust 测试覆盖。

## R7 升级迁移（方案修订 6）

旧版本的逐连接钥匙串条目，其访问控制绑定在旧的 ad-hoc 签名上，macOS 读取每个旧条目都会单独授权一次。因此改为按需迁移：启动和加载保险库只读一次主密钥，不读旧条目；某个连接第一次需要密码时才读它自己的旧条目（最多一次），迁入后删除旧条目。测试：`connections::vault::tests::registered_accounts_migrate_lazily_one_entry_per_first_use`（加载时主密钥 1 次、旧条目 0 次；首次取用时旧条目恰好 1 次）、`migrate_all_reads_each_pending_entry_once_and_dismiss_only_clears_marks`、`app::shell::tests::dismiss_stays_reachable_after_migrate_all_is_denied`、`opening_the_security_card_loads_the_vault_once_without_legacy_reads`。

## Rust 测试覆盖（按需求）

| 需求 | 测试（模块::名称） |
|---|---|
| R1 编辑/复制/粘贴/新增行 | `app::views::grid::tests::{editing_copy_takes_only_the_selected_text_and_arrows_stay_in_the_cell, editing_a_plain_cell_keeps_the_rows_below_uncovered, expanded_editor_supports_newlines_search_and_select_all, copy_has_no_header_by_default_and_shift_adds_it, pasting_whole_rows_stages_new_rows_below_the_selection, whole_row_paste_skips_the_header_and_names_the_mismatched_column, pasting_past_the_last_row_adds_new_rows, insert_goes_below_the_selected_row_and_starts_editing, new_rows_are_marked_and_can_be_discarded_one_by_one, double_click_edits_any_cell_of_a_new_row, tab_through_a_wide_new_row_keeps_the_cell_fully_visible}` |
| R3 列操作与布局 | `grid::tests::{column_layout_round_trips_per_table_and_restore_default_clears_it, remembered_layout_aligns_with_added_and_dropped_columns, hidden_primary_key_still_locates_the_preview_update, header_menu_and_panel_commands_change_the_layout, drop_indicator_matches_where_the_column_lands, column_panel_rows_offer_sort_fit_and_copy_name}`，`state::tests::{column_layouts_round_trip_through_the_file_and_are_kept_per_table, column_layout_aligns_with_dropped_and_added_columns}` |
| R4 侧栏搜索 | `app::shell::tests::{substring_search_in_a_loaded_database_shows_every_match, one_failed_connection_does_not_hide_other_results_and_reports_its_reason, truncated_database_shows_more_and_expanding_fetches_the_rest, search_quota_is_per_database_with_a_connection_cap}` |
| R5 复制 | `overview_copy_name_reaches_the_system_clipboard_and_copies_one_name_per_line` |
| R6 排序、分组、导入导出 | `connections_and_groups_reorder_locally_and_queue_one_store_write`，`connections::tests::{reordered_connections_and_groups_keep_their_order_after_reopening, legacy_store_without_positions_is_migrated_in_name_order, export_then_import_restores_groups_and_order}`；T9 新增：`drops_can_land_after_the_last_row_and_group_reorder_is_blocked_while_searching`、`queued_reorders_keep_the_latest_per_container_in_operation_order`、`importing_a_file_restores_groups_and_order_through_import_rows` |
| R7 保险库 | `connections::vault::tests::*`（往返、缓存、损坏、钥匙串拒绝、迁移 3 条失败 1 条、重启不重复迁移），`connections::tests::{the_default_connections_state_keeps_secrets_in_the_vault, first_vault_load_migrates_every_saved_legacy_entry_and_reports_failures, a_denied_keychain_or_corrupt_vault_explains_how_to_recover_when_opening}`；T9 新增：`corrupt_report_is_not_masked_by_a_failed_migration_write`、`denied_master_key_queues_a_single_key_unavailable_notice`、`retry_reports_recovered_accounts_for_the_ui`、`app::shell::tests::{vault_alert_counts_failures_and_retry_queues_one_service_job, vault_migration_failure_surfaces_in_the_ui_and_retry_calls_the_service}` |
| R8–R10/R12/R14 查询页 | `tab_overflow_list_shows_icons_env_dirty_close_and_current`，`risky_batches_wait_in_a_modal_with_pinned_buttons`，`clicking_the_risk_scrim_keeps_the_batch_and_focuses_the_modal`，`tabs_awaiting_risk_confirmation_are_marked_in_the_strip_and_the_list`，`an_edit_only_drops_errors_of_the_statement_it_touches`，`error_popover_ids_do_not_collide_with_many_errors_on_one_line`，`trailing_keywords_are_not_read_as_aliases`，`alias_qualifier_and_alias_candidates`，`auto_alias_avoids_keywords_and_taken_names`；live：`call_returns_every_result_set_and_warnings_on_the_compose_fixture`；T9 新增：`ui::tabs::tab_tests::long_titles_are_elided_in_the_middle` |
| R11 执行计划 | `tabular_three_table_join_becomes_left_deep_nested_loops`，`json_nested_loop_keeps_the_join_hierarchy`，`single_table_plan_is_one_table_node`，`arrow_tree_keeps_rows_after_indentation_falls_back`，`three_table_join_layout_has_edges_and_join_pills`，`ui_prefs_remember_the_plan_view_across_restarts`；T9 修正了 `count_text` 的进位（扩展到 `plan_canvas` 已有测试） |
| R13 主键与索引 | `mysql_create_table_writes_the_primary_key_in_pk_order`，`mysql_single_primary_key_to_composite_is_one_alter_with_drop_and_add`，`postgres_primary_key_change_drops_the_constraint_and_adds_it_back`，`auto_increment_inside_a_composite_primary_key_passes_validation`，`existing_table_primary_key_can_be_extended_reordered_and_previewed`，`index_columns_move_by_drag_position` |

## T9 修复

1. **导入恢复分组与顺序**：`import_rows` 保存完连接后调用 `restore_import_layout`。恢复失败时，原因以“分组与顺序: …”一行写进导入报告通知，不会被丢弃。导入完成后重读分组。
2. **排序写入串行化**：界面线程按操作顺序把请求放进队列。后台持锁一次取完队列，每个容器只保留最后一次，再按顺序写入。这样快速连续移动不会乱序提交。
3. **拖到最后一位**：连接行和分组行都会记录指针在上半行还是下半行。在下半行时插入线画在下沿，放手后插到该行之后。
4. **搜索时禁止分组排序**：`move_group_to` 与连接一样拒绝排序，并提示先清除搜索。搜索时分组拖动不显示插入线。
5. **计划画布计数**：四舍五入后满 1000 就进到下一个单位（`999_950` 显示为 `1M`，`999_960_000` 显示为 `1B`）。
6. **页签标题**：最小宽度保持 96px。长标题改为中间省略，保留开头和结尾，全角字符按 2 个单位计。完整标题仍在 tooltip 和无障碍名称上。
7. **保险库提示（R7）**：
   - 工作区顶部新增提示条，显示迁移失败的条目数、密钥暂不可用的原因、损坏文件的保留位置。可以“重试迁移”（调用 `Service::retry_failed_migration`），也可以收起。
   - 设置 → 本地新增“安全”卡片：说明钥匙串只保存一把主密钥，以及首次授权时应选择“始终允许”；同时显示保险库状态和重试入口。
   - 界面只读取保险库排出的事件，不会为了显示提示而访问钥匙串。
   - 重启后，之前没迁入的条目会在保险库首次加载时重新上报。
8. **损坏提示不再被掩盖**：首次加载时，如果保险库损坏后迁移写盘也失败，返回的仍是损坏说明。迁移会在下一次访问时重跑。

## 敏感名称扫描

- 用 Owner 提供的敏感名称正则对整个仓库（含 docs/，排除 `src-tauri/target` 与 `.git`）扫描：零命中。T10 改写了 5 处源码措辞，T11 改写了 16 个历史验证文档的措辞。
- 本目录截图只包含通用夹具名称（`shop`、`shop_a`、`shop_search_a/b`、`orders`、`order_lines`、`wide_orders`、`demo-uat`、`demo-dev`、`demo-prod`、`demo_user`、`演示库`）。
