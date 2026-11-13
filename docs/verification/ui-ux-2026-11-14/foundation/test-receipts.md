# 测试与命令回执（T1）

全部命令在本任务的工作区（HEAD `641d902` + 本任务改动）上于 2026-11-14 实际执行，串行运行，没有并发第二个 cargo。

## 1. 正式命令

| 命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked` | 通过。lib: 880 个用例中 **873 passed / 0 failed / 7 ignored**（22.77s）；bin 测试 1 passed |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | 通过（exit 0）。共 34 条 warning，全部是既有代码的 dead_code/unused（`kit.rs` 0 条） |
| `git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock` | 通过（exit 0），Cargo 依赖未变 |

按计划，这三条由 Delivery Owner 在集成后的同一 candidate 上重跑；本回执只证明本任务范围内通过。

## 2. 焦点用例

`cargo test --manifest-path src-tauri/Cargo.toml --lib -- ui::kit::` → **22 passed / 0 failed**：

| 用例 | 覆盖 |
| --- | --- |
| `state_tests::states_are_left_aligned_with_their_action_at_every_width` | 1360/900/640 下空态说明与主操作左对齐（说明左边距 12、主操作与标题同列 38）、说明最宽 360 不越界、主操作可点 |
| `state_tests::empty_no_match_and_error_share_one_layout_with_distinct_tones` | 三类语义的图标色调互不相同（浅/深两套），三种语义的几何完全一致 |
| `state_tests::local_loading_stays_one_line_while_first_screen_loading_fills_the_block` | 局部加载只有一行高（= `strip_h`）且贴左；首屏加载整块占满且更高 |
| `state_tests::skeleton_state_is_density_driven_and_replaces_the_full_screen_spinner` | 骨架行高随密度 26/32、行数等于请求数、不叠全屏 Spinner |
| `state_tests::row_states_are_not_tab_stops` | 反向：列表行不抢键盘焦点 |
| `metric_tests::derived_heights_follow_density_and_never_coincide` | `toolbar_h/bar_h/tools_h/strip_h` 三档取值与严格递增，反向断言不是常量 |
| `metric_tests::pane_padding_switches_on_exact_density_boundaries` | `by_density` 边界 31.9 / 32 / 39.9 / 40 |
| `metric_tests::viewport_clamping_touches_only_horizontal_padding_at_the_three_widths` | 1360/900 不钳制、640 收到紧凑水平边距，垂直边距与行高不动 |
| `metric_tests::density_global_defaults_to_compact_and_reports_real_changes` | 未发布回落紧凑；同值重复发布不重绘；改档返回 true |
| `metric_tests::palette_of_tracks_the_theme_and_dark_is_not_an_inversion` | `Palette::of` 跟随 `apply_theme`；深色不是浅色反色（结构/正文/强调色） |
| `palette_tests::*`（5 个，既有） | 两套色板对比度 4.5:1 / 焦点环 3:1 / 焦点环只描边 / 密度三档 |
| `interactive_tests::*`（4 个，既有） | Tab 停靠点、禁用不可聚焦、点击不抢焦点（浅/深） |
| `token_tests::style_literals_follow_design_tokens` | **通过**：app/ 与 ui/ 未出现裸字号/裸圆角，例外清单 6 条全部仍存在 |
| `token_tests::style_scan_rejects_reverted_literals` | 回退任一替换都会被扫描抓到 |

## 3. 回归相关的既有用例

- `app::shell::tests::connections_empty_state_wraps_inside_its_panel_at_every_width`：通过。空态改左对齐后，1360/900/640 下说明仍不压连接列表、不越窗口、宽度 ≤ 360。
- `ui::foundation::tests::*`、`ui::grid_proto::tests::*`、`app::views::*` 全部通过，未出现因三态排版变化导致的回归。

## 4. ignored 用例（不计作通过）

7 个 ignored 均为既有的 fixture/基准用例，本轮未运行、环境不可达：

`app::shell::tests::mysql_lazy_navigation_latency_fixture`、`catalog::read::latency_fixture_tests::mysql_catalog_latency_fixture`、`app::views::grid::session::tests::benchmark_grid_display_preparation`、`app::views::grid::session::tests::benchmark_grid_shared_rows`、`export::tests::benchmark_loaded_export_memory`、`grid::tests::benchmark_result_location`、`query::desk::tests::query_result_access_benchmark`。

## 5. 基准恢复校验

| 命令 | 结果 |
| --- | --- |
| `git show 727fc3a:specs/2026-10-17-desktop-ui-ux-redesign-design.md \| wc -l` | 360 |
| `md5 -q`（版本库原文 vs 读取记录去前缀） | 两侧同为 `87439bc01c87e276652c9d3851de8e1e` |

## 6. 未执行

- 原生 `rillsql` 启动与截图：本环境没有可用原生窗口/截屏通道，未执行。浅/深色 × 紧凑/标准的底座像素证据缺，需 Delivery 在集成 candidate 上补。
- 数据库 fixture（MySQL/MariaDB/PostgreSQL 容器）：本任务没有涉及连接行为，未启动。
