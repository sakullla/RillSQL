# 导入导出抽屉（T12）验证目录

- 日期：2026-11-14
- 任务：T12「统一导入导出抽屉的加载、空态、禁用语义和响应式操作，保持后台执行与文件行为」
- 覆盖需求：R1（呈现与交互与设计基准一致）、R3（四态、键盘等价、三态各带下一步）、R4（1360/900/640 布局与主操作可达）
- 覆盖规范：S2 第 6/7 条（全部控件四态、三档宽度页面本身不出现横向滚动）、S5.3（四态与光标）、S5.6（焦点环、图标按钮有 aria-label）、S8.5（空态说明发生了什么与下一步、"没有数据"与"没有命中"文案不同）、S12.3（`kit::interactive` 四态落地）、S12.4（空/加载/错误三态入口）
- 改动范围：`src-tauri/src/app/views/export/mod.rs`（来源四态、被挡住的行动作、任务空态、导入空态、响应式动作、禁用语义）、`src-tauri/src/i18n/mod.rs`（新增 12 条导入导出文案，只追加不覆盖）
- 未改：`src-tauri/src/app/shell/chrome.rs` 的抽屉层（occlude / 宽度钳制 / Esc / 外点关闭 / 内部滚动）、`src-tauri/src/export/`、`src-tauri/src/grid/` 的导出写文件路径、Cargo 依赖

## 文件

| 文件 | 内容 |
| --- | --- |
| `export-audit.md` | 逐条偏差审计（01 探索记录里的偏差 → 修复 → 验证入口），以及"没动什么"的边界清单 |
| `widths-1360-900-640.md` | 1360/900/640 × 900 三档宽度下的抽屉宽度、页边距、动作行与溢出结果 |
| `theme-density-matrix.md` | 浅/深色 × 紧凑/标准 下的四态与三态取值矩阵，以及截图的取得情况 |
| `test-receipts.md` | 本轮实际运行的命令、结果、用例清单与未取得项 |

## 证据类型（请勿混读）

- **源码级**：`SourceState` / `BlockedAction` 的判定与文案键由 `cargo test` 纯断言覆盖（`source_state_separates_running_no_table_no_result_and_ready`、`blocked_actions_name_the_reason_and_the_next_step`、`state_copy_differs_per_case_and_english_has_no_chinese`）。
- **headless 布局级**：GPUI 测试窗口里按外壳同一套几何渲染真实 `ExchangePane`（宽 520、更窄时右端留 60px 的抽屉框），逐格断言元素与可访问名称（`exchange-source-card` / `exchange-export-card` / `exchange-import-card` / `exchange-tasks` / `inline-loading` / `empty-state` / `state-action` / `exchange-source-refresh-no-table` 等），并按 1360/900/640 × 浅/深色 × 紧凑/标准 断言动作不越过抽屉右边缘（`exchange_states_hold_at_1360` / `_900` / `_640`）。
- **既有回归（未改动代码）**：后台写文件、取消、已读取行与预览信封的保护仍由 `background_export_tests` 与 `mod tests` 的既有用例覆盖（`loaded_file_write_runs_in_background_and_releases_its_snapshot`、`loaded_export_refuses_previews_and_writes_the_original_after_the_shared_read`、`cancelled_task_copy_is_not_the_running_label` 等）。
- **未取得：原生像素截图**。本轮没有原生窗口与截屏通道的使用授权，未生成 PNG；没有用历史截图替代。按计划由交付阶段在最终集成 candidate 上采集。缺口同样列在 `theme-density-matrix.md` 与 `test-receipts.md` 的"未取得项"。

## 结论摘要

- 来源现在只有四种呈现：运行中（局部加载 + 下一步说明）、没有当前表、没有可用结果、就绪；"没有表"与"没有结果"是两句不同的文案与不同的下一步（R9 的口径在本抽屉同样成立）。
- 被挡住的行动作（导出已读取的行、全量导出、全库导出、跨库迁移）不再只是一个淡按钮：每条都有自己的原因、单独的"下一步："一行，可访问名称把两句一起读出来；禁用走 `kit::interactive_states(.., true)`（不透明度 0.55、not-allowed、不可聚焦、无 hover/active）。
- 任务空态从一行灰字换成统一空态：来源就绪时主操作是"导出已读取的行"，未就绪时只说明、不摆一个点了没用的动作；"取消任务"与"清除已结束的"在没有对应任务时是禁用态。
- 查询来源下的导入区改成空态 + "切到当前表数据"动作，不再是一段没有出路的说明文字。
- 响应式：页边距按视口钳到紧凑档（`Metrics::for_viewport`），导入与任务的动作行可换行，容器补 `min_w(0) / max_w_full`，headless 断言三档宽度下动作都不越出抽屉右边缘。
- 抽屉行为不变：occlude、宽度 `min(520, 视口-60)`、Esc 关闭、外点关闭、内部 `overflow_y_scroll`、后台导入导出、取消与文件结果全部沿用原实现（审计文档给出了源码定位与本轮不动这些代码的说明）。
