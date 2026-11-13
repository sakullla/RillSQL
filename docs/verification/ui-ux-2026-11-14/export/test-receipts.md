# 测试回执（T12 导入导出抽屉）

- 工作目录：`/Users/yanggeng/github/RillSQL`
- HEAD：收到 Recipe 时为 `c3220e9`（`feat: 连接改为全局模态并收敛启动台与按需连接`）；实现与测试在随后被 Runtime 推进到 `df79f24` 的同一工作区上完成（改动未提交，仅本任务两个源码文件 + 本目录）
- 全部命令都带 `--manifest-path src-tauri/Cargo.toml --locked`，`src-tauri/Cargo.toml` 与 `src-tauri/Cargo.lock` 未改动（`git diff --exit-code` 对这两个文件为空）

## 实际运行的命令与结果

| # | 命令 | 结果 |
| --- | --- | --- |
| 1 | `cargo check --manifest-path src-tauri/Cargo.toml --locked --all-targets` | 0 error；剩余 warning 全部位于其它任务的视图文件（`app/views/mongo`、`app/views/grid`、`app/views/diagram`、`ui/editor_proto` 等），本任务两个文件无 warning |
| 2 | `cargo test --manifest-path src-tauri/Cargo.toml --locked app::views::export` | **ok. 15 passed; 0 failed**（958 filtered out） |
| 3 | `cargo test --manifest-path src-tauri/Cargo.toml --locked exchange` | **ok. 16 passed; 0 failed**（含 `app::shell::tests` 的 9 条、`app::views::grid` 的 1 条、`export::tests` 的 2 条与本任务三档宽度的 3 条） |
| 4 | `cargo test --manifest-path src-tauri/Cargo.toml --locked i18n` | ok. 1 passed（`both_locales_translate_every_catalog_key`） |
| 5 | `cargo test --manifest-path src-tauri/Cargo.toml --locked style_literals` | ok. 1 passed（`ui::kit::token_tests::style_literals_follow_design_tokens`） |
| 6 | `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | `app/views/export/mod.rs` 与 `i18n/mod.rs` 0 warning（其余告警属其它任务文件） |

**没有运行**完整 `cargo test --locked` 全量套件：按计划由 Delivery 在集成后的同一 candidate 上执行（本任务只跑与变更直接相关的过滤器）。

## 用例清单（`app::views::export`，15 条全部通过）

新增（`mod unified_state_tests`，7 条）：

- `source_state_separates_running_no_table_no_result_and_ready`：表/查询 × 有列/无列/运行中七种组合的四态判定；运行中优先于"无结果"。
- `blocked_actions_name_the_reason_and_the_next_step`：四个被挡住的行动作在查询运行中、无表、查询无结果、表读取中、查询就绪五种来源下各自的原因键；并断言原因与"下一步"是两句不同的话。
- `state_copy_differs_per_case_and_english_has_no_chinese`：12 条新文案在两语言都非空、英文不含 CJK、任意两条不重复（"没有表"≠"没有结果"）。
- `exchange_states_hold_at_1360` / `_900` / `_640`：三档宽度 × 浅/深色 × 紧凑/标准（4 组合）下逐格断言就绪/运行中/无表/无结果四态的呈现、被挡住行动作的可访问名称、任务空态与禁用按钮、以及动作不越过抽屉左右边缘；并比对抽屉宽度 = `min(520, 视口-60)`。
- `tasks_empty_state_drops_the_export_action_when_the_source_is_not_ready`：来源未就绪时任务空态不提供"导出已读取的行"动作。

既有（`mod tests` 7 条 + `background_export_tests` 1 条，全部保持通过，用于证明导入导出与文件行为没回退）：

- `switching_sources_clears_the_mapping_and_drops_late_results`
- `query_source_has_no_full_capability_and_reports_running_not_success`
- `english_exchange_copy_has_no_chinese`
- `export_range_limits_loaded_rows_and_resets_with_the_source`
- `cancelled_task_copy_is_not_the_running_label`
- `loaded_export_refuses_previews_and_writes_the_original_after_the_shared_read`
- `loaded_export_compares_protocol_shaped_text_against_the_sheet_limit`
- `loaded_file_write_runs_in_background_and_releases_its_snapshot`（后台写 10 MB 文件、期间仍可操作界面状态、完成后释放快照）

## 交叉验证（其它模块里与抽屉相关的用例，随 `exchange` 过滤器一起跑过）

- `app::shell::tests::exchange_source_buttons_route_through_the_shell`：面板的来源按钮仍走 `request_source → Workbench::perform`。
- `app::shell::tests::exchange_running_blocks_before_uncommitted_grid_or_query_draft`、`exchange_loaded_export_omits_uncommitted_cells`、`exchange_rows_are_lazy_cached_and_released_when_hidden`：导出前未提交内容、行缓存与释放的既有边界不变。
- `app::shell::tests::exchange_commands_prompt_map_cancel_and_fail_from_the_workbench`、`exchange_blocks_empty_mapping_stops_the_earlier_import_and_fails_only_that_task`：确认/取消/失败只影响该任务的既有语义不变。
- `app::views::grid::session::tests::exchange_and_export_never_see_staged_rows_or_values`：暂存行不进入导出。

## 未取得项

- **原生像素截图**（浅/深色 × 紧凑/标准、1360/900/640）：本轮没有原生窗口与截屏通道授权，未生成 PNG，也未用历史截图替代；由交付阶段在最终集成 candidate 上采集。
- **抽屉层的原生交互**（Esc、外点关闭、occlude、内部滚动）：这些代码在 `src-tauri/src/app/shell/chrome.rs`，属本任务范围外，本轮没有改动也没有新增用例；审计文档给出了源码定位。
- **新按钮的端到端结果**（"重新读取来源"、"切到当前表数据"经外壳重新绑定来源后的表现）：需要完整 `Workbench`，本轮只断言面板侧按钮存在且 id 唯一，端到端由交付阶段复核。
