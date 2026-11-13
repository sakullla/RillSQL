# 测试回执（T13 Mongo 集合文档页）

日期：2026-11-14。命令都在仓库根目录执行（`/Users/yanggeng/github/RillSQL`），工作点 `df79f24` 之上的未提交改动。本任务改动是 `src-tauri/src/app/views/mongo/mod.rs` 与本目录文档；7 条 `mongo.*` 文案已随共享 i18n 收口提交 `915cca9` 进入 `main`，本任务不重复认领 `src-tauri/src/i18n/mod.rs`。Cargo 依赖未改（`src-tauri/Cargo.toml`、`Cargo.lock` 无 diff）。

## 1. Mongo 模块自身的用例

命令：

```
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib app::views::mongo
```

结果：**16 passed; 0 failed; 0 ignored; 957 filtered out**（其中 1 个是夹具端到端用例，实际跳过，见第 4 节）。

| 用例 | 覆盖 |
| --- | --- |
| `columns_follow_fields_and_invalid_json_stays_local` | 既有：动态列、`cell_text`/`cell_badge`/`cell_fragment`、`next_skip`、`quick_validate`（空、非对象、语法错误） |
| `document_cells_take_their_states_from_the_kit` | 新增：单元格四态取值来自 `kit::interaction_style`（hover/active/accent 2px 焦点环/禁用不可聚焦），浅深两套色板 |
| `docs_states_separate_empty_no_match_and_error` | 新增：六态判定——没目标 → 空集合 → 无命中 → 读取中优先 → 读取失败不退化成空集合 → 有行；换目标清键盘光标 |
| `error_state_replaces_the_body_and_offers_retry` | 新增：读取失败画 `error-state` + `mongo-docs-retry`，不同时画 `empty-state` |
| `filtered_collection_without_match_offers_clearing_the_filter` | 新增：无命中画 `no-match-state` + `mongo-no-match-clear`，不是无数据空态 |
| `clicking_a_cell_opens_that_column_value` | 新增：点 `mongo-cell:0:customer` 打开该列片段（旧实现按列下标取字段，点不开），并同步 `focused_cell` |
| `keyboard_moves_between_cells_and_enter_opens_the_focused_one` | 新增：Tab 进数据区后 ↓/→/End/← 移格、Enter 打开、Esc 收起只读查看 |
| `narrow_pane_keeps_document_columns_readable` | 扩展：640 下 7 列每列 ≥ 120、行宽 ≥ 964，同时页面根 ≤ 640、数据区 ≤ 640、左栏默认收起 |
| `narrow_navigation_overlays_instead_of_squeezing_the_table` | 新增：640 展开左栏是抽屉（`mongo-nav` + `mongo-nav-backdrop`），文档区宽度不变（±2），点遮罩收回，按钮文案变"折叠导航" |
| `mongo_write_is_previewed_and_target_switch_drops_it` | 既有：新增先出确认条、确认条带目标与 JSON、换集合丢弃旧预览与草稿 |
| `docs_states_and_cells_hold_at_1360` | 新增：1360 × 900 × 浅/深 × 紧凑/标准，六态 + 单元格点击 + 键盘 + 宽度关系 |
| `docs_states_and_cells_hold_at_900` | 同上（900） |
| `docs_states_and_cells_hold_at_640` | 同上（640，左栏收起 + 数据区内部横向滚动） |
| `live::rendered_controls_keep_invalid_json_and_unconfirmed_delete` | 既有、无需夹具：无效 JSON 留在编辑器并报错、未确认删除后文档仍在、`mongo-view:0` 能看到文档 JSON（FormatViewer 未回退） |
| `live::sidebar_mongo_button_opens_the_browser` | 既有、无需夹具：侧栏入口打开集合文档页，未连接时不显示成功 |
| `live::live_mongo_pages_filters_inserts_replaces_and_deletes` | 既有、**需要 MongoDB 夹具：本轮跳过，不算通过**（见第 4 节） |

## 2. 外壳侧的相邻回归

命令：

```
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib app::shell::tests
```

结果：**129 passed; 0 failed; 1 ignored; 843 filtered out**（该 ignored 不是本任务新增）。其中与本任务直接相关的用例：

- `switching_work_items_drops_structure_admin_redis_and_mongo_state`：切换工作项后 Mongo 目标重置（换集合不保留旧写入预览、编辑器关闭、草稿清空）。

## 3. 编译与静态检查

```
cargo check --manifest-path src-tauri/Cargo.toml --locked --all-targets
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked
```

- `cargo check`：通过。输出中的告警都在其它模块（`src/diagram`、`src/app/shell/layout.rs`、`src/app/views/grid`、`src/redis`、`src/ui/format_viewer.rs` 的既有 dead_code/unused），与本次改动无关。
- `cargo clippy`：与 `src/app/views/mongo/`、`src-tauri/src/i18n/mod.rs` 相关的告警为 **0**。

两条与本改动直接相关的门禁用例：

```
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib style_literals_follow_design_tokens   # 1 passed
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib i18n::tests                            # 1 passed
```

- `style_literals_follow_design_tokens`：通过（新增代码只用 `TypeScale` / `Radius` / `Metrics` 与既有例外，未新增裸字号或胶囊圆角）。
- `i18n::tests::both_locales_translate_every_catalog_key`：通过（新增 7 条 `mongo.*` 文案两种语言都有取值）。

## 4. 未取得项（不能算通过）

1. **MongoDB 夹具端到端**：`live::live_mongo_pages_filters_inserts_replaces_and_deletes` 需要本机 MongoDB 夹具（`docker/compose.yaml`）。本机没有 `docker` 命令（`command not found: docker`），用例输出：

   ```
   跳过：MongoDB 夹具没有连上：MongoDB 会话没有建立
   test app::views::mongo::live::live_mongo_pages_filters_inserts_replaces_and_deletes ... ok
   ```

   它在测试框架里计为 ok，但**没有任何断言执行**，因此不构成本轮证据。分页、过滤、新增/替换/删除落库的真实链路仍需在夹具可用时重跑。
2. **原生 1360/900/640 像素截图、浅/深 × 紧凑/标准截图**：本轮没有原生窗口与截屏通道的使用授权，未生成 PNG，不用历史截图替代。按计划由交付阶段在最终集成 candidate 上采集（`docs/verification/` 入口汇总见交付报告）。
3. **全量 `cargo test --locked`**：按工作流分工由 Delivery 在同一 candidate 上执行一次；本任务只跑与改动直接相关的模块过滤器（第 1、2 节），不冒称全量通过。
