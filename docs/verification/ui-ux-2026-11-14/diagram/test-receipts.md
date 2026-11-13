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

### 运行时间与“挂住”事故（2026-11-14 04:xx 追加）

早前一版把这些断言写成一个用例 `four_states_and_keyboard_hold_at_three_widths_and_both_themes`：在**同一个测试里循环开 3 个窗口**，再用 `Window::resize` 改运行期尺寸。该版本在一个测试进程里跑 28 分钟不退出（CPU 持续 200%），把别人的 crate 级 `cargo test` 一起堵住。

已修复（源码里已经不存在的用例名就是证据：`grep -c four_states_and_keyboard_hold_at_three_widths_and_both_themes` → `0`）：

| 措施 | 修复前 | 修复后 |
| --- | --- | --- |
| 窗口数量 | 一个用例里 3 个窗口 | 每个宽度一个用例、**每个用例只开 1 个窗口** |
| 视口尺寸来源 | `Window::resize`（实测不改视口） | 开窗时给 `WindowBounds::Windowed`，全程不改尺寸；源码里已无 `resize(` 调用 |
| 键盘驱动等待 | 最多 40 次 Tab+Enter | 最多 12 次（页头 6 个停靠点 + 2 张卡片），并额外加 15 秒墙钟上限，卡住只会失败不会挂起 |

根因说明：能确认的事实是①挂住的进程跑的是旧用例名，而该名字已不在源码里（改动后被替换）；②旧写法在同一个用例内循环开 3 个窗口、并用 `Window::resize` 改运行期尺寸，而 `resize` 实测并不改视口（也就是说它只触发了重新布局却没有得到新视口）；③新写法每个用例只开一个窗口、不调用 `resize`、所有等待都有次数上限 + 墙钟上限，重复跑都正常退出。旧写法里究竟卡在开第 2 个窗口还是卡在 `resize` 的重新布局循环，没有拿到栈，不当作已验证根因——但两条路径现在都已不在用例里。

修复后的实测（本机，同一仓库）：

| 命令 | 耗时 | 结果 |
| --- | --- | --- |
| `cargo test --locked --lib app::views::diagram`（并行，连跑 5 次） | 每次 wall 0.96–1.03 s，用例内 0.48–0.53 s | 5/5 全绿，10 passed |
| 同上 `-- --test-threads=1`（连跑 3 次） | 用例内 1.59–3.25 s | 3/3 全绿 |
| `cargo test --locked --lib`（全量） | wall 41.65 s / user 104.40 s | 931 passed / 0 failed / 7 ignored，EXIT=0 |

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
- 第三次全量（修复窗口用例 + 邻接任务再提交后）：931 passed / 0 failed / 7 ignored，wall 41.65 s，EXIT=0。
- Clippy（`--all-targets`）：退出码 0。与关系图相关的两条 warning 都是本轮之前就存在的（`src/diagram/mod.rs` 的 `DiagramColumn` re-export、`app/views/diagram/mod.rs` 测试模块里的 `AppContext` 导入），本轮新增代码没有引入 warning。

## 6. 基线与归属说明

- 最终复跑在 `3c21298`（结构页文案跟进提交）之上的工作区完成：`cargo test --locked` → **930 passed / 0 failed / 7 ignored**（`EXIT=0`）。
- 本任务新增的 3 条 `i18n` 文案（`diagram.emptyDetail`、`diagram.searchNoMatch`、`diagram.searchClear`）在提交 `3c21298` 里被一并带入 `main`；关系图实现本身（`src-tauri/src/app/views/diagram/mod.rs`）仍在工作区，等待本任务提交。集成申报时请不要重复认领 `src-tauri/src/i18n/mod.rs`。

## 未取得项

- **原生像素截图**：浅/深色 × 紧凑/标准、1360/900/640 的 PNG 都没有取得。本轮无原生窗口与截屏通道授权，未用历史截图替代；交付阶段在最终集成 candidate 上补齐。
- 7 个 `ignored` 用例（全量里的既有 ignored，含需要真实数据库或平台的用例）未计入通过数；关系图相关用例不在其中。
