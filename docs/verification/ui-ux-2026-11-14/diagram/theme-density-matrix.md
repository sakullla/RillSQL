# 浅/深色 × 紧凑/标准 的关系图取值矩阵（T10）

证据类型：**源码级 + headless 布局级**，不是原生像素截图（见 `README.md`）。所有取值来自 `src-tauri/src/ui/kit.rs` 的 `Palette::{light,dark}` 与 `Metrics::{compact,standard}`；关系图只引用这些令牌，没有自己的字号、圆角或颜色常量。

## 1. 卡片四态（hover / active / focus-visible / disabled）

四态由 `kit::interactive(card, &p, false)` 提供，取值即 `kit::interaction_style(&p, false)`：

| 项目 | 浅色 | 深色 | 尺寸来源 |
| --- | --- | --- | --- |
| hover 底色 | `hover` #eceef3 | `hover` #2a2d34 | 色板 |
| 按下底色 | `active` #e2e5ec | `active` #33363e | 色板 |
| 焦点环 | `accent` #4f5bd5，2px 内描边 | `accent` #7c86f5，2px 内描边 | `kit::FOCUS_RING_W = 2` |
| 卡片底色 / 边框 | `surface` / `line_strong` | `surface` / `line_strong` | 色板 |
| 悬停（hot）表头底 / 边框 | `accent_soft` / `accent` | `accent_soft` / `accent` | 色板 |
| 关联（linked）边框 | `accent` | `accent` | 色板 |
| 其余卡片 dim | 不透明度 0.45 | 0.45 | 既有行为，未改 |
| 卡片圆角 / 阴影 | `Radius::PANEL` / `shadow_sm` | 同左 | 令牌 |
| 卡片宽度 / 表头高 / 行列高 | 220 × zoom、34 × zoom、22 × zoom | 同左 | 图布局常量（不随密度变，图是画布） |
| 列名 / 类型字号 | `TypeScale::SM` / `TypeScale::XS` | 同左 | 令牌 |

**密度**只影响页面外壳（页头高 `header_h`、页边距 `page_pad`、控件高 `control_h`），不影响画布内部的图元尺寸——ERC 图这类画布在紧凑/标准下保持同一张图，避免切换密度时卡片错位。

## 2. 四种状态（Loading / Error / NeedTarget / Empty）

| 项目 | 浅色 · 紧凑 | 浅色 · 标准 | 深色 · 紧凑 | 深色 · 标准 |
| --- | --- | --- | --- | --- |
| 错误图标块底 / 图标 | `danger_soft` / `danger` | 同左 | `danger_soft` / `danger` | 同左 |
| 空态（无目标 / 空图）图标块底 / 图标 | `accent_soft` / `accent_text` | 同左 | `accent_soft` / `accent_text` | 同左 |
| 标题字号 / 说明字号 | `TypeScale::MD` / `SM` | 同左 | 同左 | 同左 |
| 动作位左缩进 | 38 | 38 | 38 | 38 |
| 说明最大宽度 | 360 | 360 | 360 | 360 |
| 页边距（水平） | 16 | 20 | 16 | 20 |
| 页头高度 | 42 | 48 | 42 | 48 |
| 加载条字号 | `TypeScale::SM` | 同左 | 同左 | 同左 |

主题只决定色板，密度只决定尺寸；三态排版在四种组合下是同一份几何（`state_block`）。

## 3. 由哪些用例钉住

- 四种组合 × 三个宽度：`diagram_four_states_and_keyboard_hold_at_1360`、`_900`、`_640`（每格都断言卡片、页头行、缩放、Tab+Enter、四态元素）。
- 四态取值：`card_keyboard_equivalents_match_the_mouse_gestures`（`hover_bg == p.hover`、`active_bg == p.active`、`ring == p.accent`、`ring_width == kit::FOCUS_RING_W`）。
- 令牌底线：T1 的 `style_literals_follow_design_tokens` 继续扫描 `app/views`，关系图没有新增裸字号/裸圆角。

## 4. 截图取得情况

**未取得**原生 浅/深色 × 紧凑/标准 像素截图。原因与替代材料见 `README.md`「证据类型」：本轮无原生窗口与截屏通道授权，未用历史截图替代；交付阶段在最终集成 candidate 上补齐。
