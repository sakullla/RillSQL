# 浅/深色 × 紧凑/标准 的 Mongo 文档区取值矩阵（T13）

证据类型：**源码级 + headless 布局级**，不是原生像素截图（见 `README.md`）。所有取值来自 `src-tauri/src/ui/kit.rs` 的 `Palette::{light,dark}` 与 `Metrics::{compact,standard}`；Mongo 页只引用这些令牌，没有自己的字号、圆角或颜色常量。

## 1. 单元格与行的四态（hover / active / focus-visible / disabled）

四态由 `kit::interactive(cell, &p, false)` 提供，取值即 `kit::interaction_style(&p, false)`：

| 项目 | 浅色 | 深色 | 尺寸来源 |
| --- | --- | --- | --- |
| hover 底色 | `hover` #eceef3 | `hover` #2a2d34 | 色板 |
| 按下底色 | `active` #e2e5ec | `active` #33363e | 色板 |
| 焦点环 | `accent` #4f5bd5，2px 内描边 | `accent` #7c86f5，2px 内描边 | `kit::FOCUS_RING_W = 2` |
| 光标 | `pointer`（可点格） | 同左 | 底座 |
| 行 hover 底色 | `hover` | `hover` | 色板 |
| 禁用 | 单元格不提供禁用路径；禁用时底座给 0.55 不透明度、`not-allowed`、无 hover/active、不可聚焦 | 同左 | `interaction_style` |
| 单元格圆角 | `Radius::CONTROL`（4） | 同左 | 令牌 |
| 数据结构徽标 | `pill(accent_text, accent_soft)` | `pill(#aab1ff, #262a45)` | 色板 |
| `_id` 列文字 | `text_muted` + 等宽字体 | 同左 | 令牌 |
| `null`/空值文字 | `text_faint` | 同左 | 令牌 |

## 2. 六态的取值（NeedTarget / Loading / Error / EmptyCollection / NoMatch / Rows）

| 项目 | 浅色 · 紧凑 | 浅色 · 标准 | 深色 · 紧凑 | 深色 · 标准 |
| --- | --- | --- | --- | --- |
| 错误图标块底 / 图标 | `danger_soft` / `danger` | 同左 | `danger_soft` #3d1e20 / `danger` #f06d6d | 同左 |
| 空集合图标块底 / 图标 | `accent_soft` / `accent_text` | 同左 | `accent_soft` #262a45 / `accent_text` #aab1ff | 同左 |
| 无命中图标块底 / 图标 | `hover` / `text_muted` | 同左 | `hover` / `text_muted` | 同左 |
| 标题 / 说明字号 | `TypeScale::MD` / `SM` | 同左 | 同左 | 同左 |
| 动作位左缩进 | 38 | 38 | 38 | 38 |
| 说明最大宽度 | 360 | 360 | 360 | 360 |
| 行高（`m.row_h`）→ 行最小高 `row_h + 2` | 26 → 28 | 32 → 34 | 28 | 34 |
| 表头高 `m.bar_h() - 2`（`tabs_h - 4`） | 28 | 32 | 28 | 32 |
| 状态条高 `m.tree_row_h` | 23 | 30 | 23 | 30 |
| 数据区内边距 `m.pane_pad()` | 10 | 12 | 10 | 12 |
| 左栏宽 `m.header_h()*4 + pane_pad` | 178 | 204 | 178 | 204 |

- 主题只决定色板，密度只决定尺寸；六态排版在四种组合下是同一份几何（`state_block`）。
- 骨架（`skeleton_state`）占位条高 = `row_h - 10`，随密度变；不随主题变。
- 左栏宽随密度变：紧凑 178、标准 204（`Metrics::header_h` 42/48 与 `pane_pad` 10/12），这决定了 1360/900 下数据区的可用宽。

## 3. 由哪些用例钉住

- 四种组合 × 三个宽度：`docs_states_and_cells_hold_at_1360`、`_900`、`_640`。每个格子里都重跑：六态元素、单元格点击、方向键/Enter/Esc、页面根宽与数据区宽。
- 四态取值本身：`document_cells_take_their_states_from_the_kit`（浅/深两套色板下断言 hover=**`p.hover`**、active=`p.active`、ring=`p.accent` 且宽 2px、可用；禁用态无 hover/active/ring 且不可聚焦）。
- 窄宽抽屉：`narrow_navigation_overlays_instead_of_squeezing_the_table`（遮罩、点遮罩收回、文档区宽度不变、按钮文案随状态变）。
- 列宽与内部滚动：`narrow_pane_keeps_document_columns_readable`（7 列 × 120 + 动作列 124 = 964，页面根与数据区 ≤ 640）。

## 4. 未取得项

- 浅/深色 × 紧凑/标准的**原生像素截图**：本轮没有原生窗口与截屏通道授权，未生成 PNG。四种组合目前的证据是 headless 渲染断言 + 色板/密度令牌取值，不冒充实机截图。按计划由交付阶段在最终集成 candidate 上采集。
