# 测试回执（T10 关系图页）

日期：2026-11-14。命令都在仓库根目录执行（`/Users/yanggeng/github/RillSQL`）。本任务只改 `src-tauri/src/app/views/diagram/mod.rs` 与 `src-tauri/src/i18n/mod.rs`（3 条关系图文案），Cargo 依赖未改。

## 1. 关系图自身的用例

命令：

```
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib app::views::diagram
```

结果：**10 passed; 0 failed; 0 ignored**（927 filtered out）。

| 用例 | 覆盖 |
| --- | --- |
| `card_keyboard_equivalents_match_the_mouse_gestures` | Enter / Shift+Enter / 菜单键 / Shift+F10 / Esc 的等价语义；未知表不产生打开请求；四态取值来自 `kit::interaction_style`（hover、active、2px 强调色焦点环、可聚焦） |
| `pane_keys_zoom_and_dismiss_without_touching_the_diagram` | 页面级 `+`/`-`/`0` 缩放与复位、Esc 收起菜单与悬停、无关键不吞 |
| `four_states_carry_the_matching_next_step` | `PaneState` 五种判定；空图说明为什么空；失败带原因；无目标不是“读取失败” |
| `diagram_four_states_and_keyboard_hold_at_1360` | 1360 × 900：卡片、单行页头、缩放按钮、Tab+Enter、搜索无命中提示条与清除、四态 |
| `diagram_four_states_and_keyboard_hold_at_900` | 900 × 900：同上 |
| `diagram_four_states_and_keyboard_hold_at_640` | 640 × 900：`diagram-tools` 第二行、卡片、缩放、Tab+Enter、四态 |
| `sqlite_diagram_shows_cards_wires_hover_and_does_not_write` | 真实 SQLite 夹具：卡片、连线、悬停关联（hot/linked/dim）、双击打开请求，读取前后 `sqlite_master` 与 `orders` 行数一致 |
| `typed_columns_zoom_and_open_request_do_not_run_sql` | 列类型/主键、缩放钳制、打开请求不执行 SQL |
| `failure_text_names_the_target_and_retry_stays_available` | 失败文案带目标，清空目标后重试不再可用 |
| `object_spec_skips_non_relational_engines` | Redis/Mongo 不产生关系图目标 |

三个宽度用例各自在浅·紧凑、浅·标准、深·紧凑、深·标准 四组主题/密度下重跑同一份断言。

## 2. 名字里带 diagram 的全部用例（含其它模块）

命令：

```
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib -- diagram
```

结果：**20 passed; 0 failed; 0 ignored**（含 `app::shell::tests::diagram_and_specialized_engines_use_their_own_scope`、`diagram::tests::*` 七个读取/范围用例、`app::views::compare::live::live_engines_compare_a_made_difference_and_read_the_diagram`）。

## 3. 底座令牌扫描

命令：

```
cargo test --manifest-path src-tauri/Cargo.toml --locked --lib style_literals_follow_design_tokens
```

结果：**1 passed; 0 failed**。关系图没有新增裸字号/裸圆角。

## 4. 依赖边界

命令：

```
git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock
```

结果：**通过（无差异）**。

## 5. 全量测试与 Clippy

```
cargo test --manifest-path src-tauri/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked
```

本轮结果见下方「全量回执」。工作区里同时有 T3（`app/shell/**`）与 T11（`app/views/compare/`）的进行中改动，全量数字包含邻接任务未提交的代码，读回执时请区分归属。

- 第一次全量（T3 的 `conn_modal`/chrome 改动尚未自洽时）：926 passed / 4 failed，失败全部在 `app::shell::conn_modal::tests::tab_order_stays_inside_the_modal_and_wraps`、`app::shell::tests::density_theme_and_language_reach_the_rendered_chrome`、`app::shell::tests::long_copy_is_truncated_but_fully_available_and_never_holds_neighbours`、`app::shell::tests::sidebar_copy_and_test_remain_usable_while_catalog_is_loading`——均属 T3 范围，与关系图无关。
- 第二次全量（T3 修好后）：930 passed / 0 failed / 7 ignored（`TEST_EXIT=0`）。
- Clippy（`--all-targets`）：退出码 0。与关系图相关的两条 warning 都是本轮之前就存在的（`src/diagram/mod.rs` 的 `DiagramColumn` re-export、`app/views/diagram/mod.rs` 测试模块里的 `AppContext` 导入），本轮新增代码没有引入 warning。

## 6. 基线与归属说明

- 最终复跑在 `3c21298`（结构页文案跟进提交）之上的工作区完成：`cargo test --locked` → **930 passed / 0 failed / 7 ignored**（`EXIT=0`）。
- 本任务新增的 3 条 `i18n` 文案（`diagram.emptyDetail`、`diagram.searchNoMatch`、`diagram.searchClear`）在提交 `3c21298` 里被一并带入 `main`；关系图实现本身（`src-tauri/src/app/views/diagram/mod.rs`）仍在工作区，等待本任务提交。集成申报时请不要重复认领 `src-tauri/src/i18n/mod.rs`。

## 未取得项

- **原生像素截图**：浅/深色 × 紧凑/标准、1360/900/640 的 PNG 都没有取得。本轮无原生窗口与截屏通道授权，未用历史截图替代；交付阶段在最终集成 candidate 上补齐。
- 7 个 `ignored` 用例（全量里的既有 ignored，含需要真实数据库或平台的用例）未计入通过数；关系图相关用例不在其中。
