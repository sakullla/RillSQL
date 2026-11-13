# 浅/深色 × 紧凑/标准 底座取值矩阵（T1）

证据类型：**源码级 + headless 布局级**，不是原生像素截图（见 `README.md`）。取值直接从 `src-tauri/src/ui/kit.rs` 的 `Palette::{light,dark}` 与 `Metrics::{compact,standard,relaxed}` 读出，并由下表右列用例钉住；改任一数值都会让对应用例失败。

## 1. 色板（S4.3 / S4.4）

两套色板分别调过，深色不是浅色反色（`palette_of_tracks_the_theme_and_dark_is_not_an_inversion`）。

| 用途（字段） | 浅色 | 深色 |
| --- | --- | --- |
| 工作区 `surface` | #ffffff | #17181c |
| 外壳 `chrome` | #f6f7f9 | #1d1f24 |
| 活动栏 `rail` | #eef0f4 | #15161a |
| 浮层 `raised` | #ffffff | #24262c |
| 凹陷 `sunken` | #e8edf5 | #101216 |
| 悬停 `hover` | #eceef3 | #2a2d34 |
| 按下 `active` | #e2e5ec | #33363e |
| 发丝线 `line` | #e4e7ec | #2c2f36 |
| 强线 `line_strong` | #d0d5dd | #3b3f48 |
| 正文 `text` | #16191f | #e9ebef |
| 次要 `text_muted` | #4f5a68 | #a1a7b3 |
| 弱文 `text_faint` | #5b6573 | #98a0ac |
| 强调 `accent` | #4f5bd5 | #7c86f5 |
| 强调悬停 `accent_hover` | #4350c4 | #949cf8 |
| 强调按下 `accent_active` | #3a45ad | #6670e6 |
| 强调浅底 `accent_soft` | #eceefc | #262a45 |
| 强调文字 `accent_text` | #3a45ad | #aab1ff |
| 选中底 `selection` | rgba(79,91,213,.14) | rgba(124,134,245,.22) |
| 斑马 `zebra` | #fafbfc | #1a1b20 |
| 成功 `ok` / 浅底 `ok_soft` | #0f7a4d / #e6f6ee | #3ccf8e / #16352a |
| 警告 `warn` / 浅底 `warn_soft` | #8f5e14 / #fdf3e1 | #f0b44c / #3a2f17 |
| 危险 `danger` / 浅底 `danger_soft` | #b42f2f / #fdecec | #f06d6d / #3d1e20 |
| 信息 `info` | #2568b0 | #5eaaf5 |

对比度由 `palette_tests::every_theme_keeps_text_readable`、`every_theme_keeps_faint_text_readable_on_selected_backgrounds`、`interactive_states_have_visible_focus_ring_in_both_themes` 钉住：正文/次要/弱文在 surface/chrome/raised/sunken 及选中、悬停、强调浅底上 ≥ 4.5:1；焦点环在这些底上 ≥ 3:1。

## 2. 三态在四种组合下的呈现

| 项目 | 浅色 · 紧凑 | 浅色 · 标准 | 深色 · 紧凑 | 深色 · 标准 |
| --- | --- | --- | --- | --- |
| 无数据图标块底 / 图标 | `accent_soft` #eceefc / #3a45ad | 同左 | #262a45 / #aab1ff | 同左 |
| 无命中图标块底 / 图标 | `hover` / `text_muted` | 同左 | `hover` / `text_muted` | 同左 |
| 错误图标块底 / 图标 | `danger_soft` / `danger` | 同左 | `danger_soft` / `danger` | 同左 |
| 标题字号 | `TypeScale::MD` 13 | 13 | 13 | 13 |
| 说明字号 | `TypeScale::SM` 12 | 12 | 12 | 12 |
| 图标块边长 / 图标字号 | 30 / 16 | 30 / 16 | 30 / 16 | 30 / 16 |
| 说明最大宽度 | 360 | 360 | 360 | 360 |
| 主操作与标题文字左缩进 | 38 | 38 | 38 | 38 |
| 骨架行高 | `row_h` 26 | 32 | 26 | 32 |
| 局部加载行高 | `strip_h` 20 | 22 | 20 | 22 |

主题只影响色板（`Palette`），密度只影响尺寸（`Metrics`），二者不交叉；三态排版在四种组合下是同一份几何（`empty_no_match_and_error_share_one_layout_with_distinct_tones`）。

## 3. 密度三档（S9 / S12.2）

| 指标 | 紧凑 | 标准 | 宽松 |
| --- | --- | --- | --- |
| 行 `row_h` | 26 | 32 | 40 |
| 树行 `tree_row_h` | 23 | 30 | 36 |
| 菜单行 `menu_row_h` | 24 | 28 | 34 |
| 控件 `control_h` | 26 | 30 | 32 |
| 顶栏 `topbar_h` | 40 | 44 | 48 |
| 标签条 `tabs_h` | 32 | 36 | 40 |
| 状态栏 `status_h` | 24 | 26 | 28 |
| 根内边距 `pad` | 4 | 8 | 12 |
| 页面左右 `page_pad` | 16 | 20 | 26 |
| 页面上下 `page_pad_y` | 14 | 18 | 24 |
| 行上下 `row_pad_y` | 5 | 7 | 9 |
| 派生 `toolbar_h` | 38 | 42 | 46 |
| 派生 `bar_h` | 30 | 34 | 38 |
| 派生 `tools_h` | 36 | 40 | 44 |
| 派生 `strip_h` | 20 | 22 | 24 |
| `pane_pad` / `section_gap` / `card_pad` | 10 / 12 / 12 | 12 / 16 / 14 | 16 / 20 / 18 |
| 页头 `header_h` | 42 | 48 | 54 |

钉住用例：`density_metrics_are_distinct_and_ordered`、`derived_heights_follow_density_and_never_coincide`、`pane_padding_switches_on_exact_density_boundaries`、`density_global_defaults_to_compact_and_reports_real_changes`。

## 4. 三档宽度行为（1360 / 900 / 640）

| 宽度 | 水平页边距 | 三态布局（headless 实测） | 断言 |
| --- | --- | --- | --- |
| 1360 | 按密度原值（标准 20 / 宽松 26） | 说明左边距 12、宽 360、右缘 372 ≤ 1360 | `states_are_left_aligned_with_their_action_at_every_width` |
| 900 | 按密度原值 | 同上，右缘 372 ≤ 900 | 同上 |
| 640 | 钳到 `Metrics::compact().page_pad` = 16（< 760 才钳制） | 说明右缘 372 ≤ 640，主操作仍在同列 | 同上 + `viewport_clamping_touches_only_horizontal_padding_at_the_three_widths` |

- 钳制只作用于水平页边距：垂直页边距、行高、树行高在 640 下保持不变（反向断言已在用例中）。
- 边界用 `NARROW_PAGE_WIDTH` = 760：760 不钳制，759 钳制。
- 页面级「三档无横向滚动」的像素结论仍需原生窗口证据，本任务只证明底座元素在这些宽度下不越界。

## 5. 复现

~~~
cargo test --manifest-path src-tauri/Cargo.toml --lib -- ui::kit::
~~~

矩阵中的每个数值都有对应用例；任何一项被改动，上表右列的用例会先失败。
