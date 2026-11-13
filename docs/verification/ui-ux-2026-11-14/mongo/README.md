# Mongo 集合文档页（T13）验证目录

- 日期：2026-11-14
- 任务：T13「统一 Mongo 文档浏览的状态、单元格交互和窄宽导航，保持文档命令与确认行为」
- 覆盖需求：R1（呈现与交互与设计基准一致）、R3（四态、键盘等价、菜单/命令一致）、R4（三态语义、空态替换内容区、1360/900/640 页面无横向滚动）
- 覆盖规范：S5.3（四态与光标）、S5.6（可 Tab 到达、图标按钮有 aria-label）、S6.4（滚动按区域划分，页面不横向滚动）、S7.3（断点：窄窗口侧栏以抽屉覆盖内容区）、S8.5（空态讲清发生了什么与下一步）、S12.3（`kit::interactive` 四态落地）、S12.4（三态入口）
- 本次改动：`src-tauri/src/app/views/mongo/mod.rs`（文档区六态、单元格四态与键盘等价、窄宽抽屉与页面滚动职责）+ 本目录文档；新增的 7 条 `mongo.*` 文案（`mongo.nav.expand`、`mongo.tab.needConnectionHint`、`mongo.tab.needCollectionHint`、`mongo.tab.loadFailed`、`mongo.tab.refreshDatabases`、`mongo.tab.emptyCollectionHint`、`mongo.tab.emptyFilteredHint`）随共享 i18n 收口提交 `915cca9` 进入 `main`，本任务不重复认领 `src-tauri/src/i18n/mod.rs`
- 未改：`src-tauri/src/application/mongo.rs`（读写与校验）、`src-tauri/src/mongo/`（驱动）、`src-tauri/src/app/shell/`（外壳与页面切换）、`src-tauri/src/ui/`（格式查看器）、Cargo 依赖

## 文件

| 文件 | 内容 |
| --- | --- |
| `mongo-audit.md` | 逐条偏差审计（源码入口—偏差—修复结果—验证材料），含"保持不回退"的边界说明 |
| `widths-1360-900-640.md` | 三档宽度下页面根/数据区/行宽、左栏形态、单元格与键盘、六态证据 |
| `theme-density-matrix.md` | 浅/深色 × 紧凑/标准 下六态与单元格四态的取值来源，以及截图的取得情况 |
| `test-receipts.md` | 本轮实际运行的命令、结果、用例清单与未取得项 |

## 证据类型（请勿混读）

- **源码级**：`DocsState` 六态判定（`docs_state`）、`cell_key` 的键盘语义、单元格四态取值来自 `kit::interaction_style`，由 `cargo test` 纯断言覆盖（`docs_states_separate_empty_no_match_and_error`、`document_cells_take_their_states_from_the_kit`）。
- **headless 布局级**：GPUI 测试窗口里真实渲染出来的元素与可访问名称，按 1360/900/640 × 浅/深色 × 紧凑/标准 逐格断言（`docs_states_and_cells_hold_at_1360 / _900 / _640`、`narrow_pane_keeps_document_columns_readable`、`narrow_navigation_overlays_instead_of_squeezing_the_table`）。
- **无夹具回归级**：不需要数据库的既有行为用例（`rendered_controls_keep_invalid_json_and_unconfirmed_delete`、`mongo_write_is_previewed_and_target_switch_drops_it`）覆盖无效 JSON、未确认删除、未确认写入、目标切换丢弃旧预览与 FormatViewer 查看。
- **未取得：MongoDB 夹具端到端**。本机没有 `docker` 命令，`live_mongo_pages_filters_inserts_replaces_and_deletes` 打印「跳过：MongoDB 夹具没有连上：MongoDB 会话没有建立」后返回。该用例在测试结果里计为 ok，但**不算通过**，见 `test-receipts.md` 的「未取得项」。
- **未取得：原生像素截图**。本轮没有原生窗口与截屏通道的使用授权，未生成 PNG。按计划由交付阶段在最终集成 candidate 上采集，不用历史截图替代。

## 结论摘要

- 文档区现在有六态：没目标 / 读取中 / 读取失败 / 空集合 / 筛选无命中 / 有行。空集合给"新增文档"，无命中给"清除过滤"，读取失败给原因与"重试"，没目标给"重读库与集合"或 `Ctrl+B` 的键位提示。空集合与无命中文案不同，错误不再显示成"这个集合没有文档"。
- 文档单元格有 hover 底色、按下底色、键盘焦点环（2px 强调色）与光标，`role=Button` + `aria-label`；鼠标点击与键盘 Enter/空格等价，都会按**字段名**打开该列片段——旧实现把列下标当字段名，点单元格实际打不开。
- 整张文档表是一个 Tab 停靠点，方向键 / Home / End / Enter / 空格 / Esc 在表内移动与打开，不再靠 20×7 个 Tab 停靠点走格子。
- 页面根有 `overflow_hidden` 与 `min_w(0)`：横向滚动只发生在文档数据区内部；窄于 760 时展开左栏是覆盖内容区的抽屉（带遮罩、点遮罩收回），不再挤压文档表。
- 文档命令与确认行为未变：新增/替换仍先进确认条，删除仍先确认，无效 JSON 仍留在编辑器里报错，FormatViewer 仍是只读/可编辑两种模式，换目标仍清空旧预览。
