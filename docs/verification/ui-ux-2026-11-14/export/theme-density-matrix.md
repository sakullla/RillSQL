# 浅/深色 × 紧凑/标准 的导入导出抽屉取值矩阵（T12）

证据类型：**源码级 + headless 布局级**，不是原生像素截图（见 `README.md`）。所有取值来自 `src-tauri/src/ui/kit.rs` 的 `Palette::{light,dark}` 与 `Metrics::{compact,standard}`；本页只引用这些令牌，没有自己的字号、圆角或颜色常量（`style_literals_follow_design_tokens` 继续扫描 `app/views`）。

## 1. 四态与禁用

| 项目 | 浅色 | 深色 | 来源 |
| --- | --- | --- | --- |
| 行动作 hover 边缘 / 底色 | `accent` #4f5bd5 / `accent_soft` #eceefc | `accent` #7c86f5 / `accent_soft` #262a45 | `action_row` 的 `hover` |
| 行动作按下底色 | `active` #e2e5ec | `active` #33363e | `kit::interactive_states` |
| 行动作焦点环 | `accent` #4f5bd5，2px 内描边 | `accent` #7c86f5，2px 内描边 | `kit::FOCUS_RING_W = 2` |
| 被挡住的行不透明度 | 0.55 | 0.55 | `kit::interaction_style(p, true)` |
| 被挡住的行光标 | `not-allowed` | `not-allowed` | 同上 |
| 被挡住的行 Tab 可达 | 否（不设 `tab_index`） | 否 | 同上 |
| 被挡住的标题 / 原因 / 下一步 | `text_muted` #4f5a68 / `text_faint` #5b6573 / `text_faint` | `text_muted` #a1a7b3 / `text_faint` #98a0ac / `text_faint` | `blocked_row` |
| 图标块底色 / 图标 | `hover` #eceef3 / `text_faint` | `hover` #2a2d34 / `text_faint` | `blocked_row` |
| 空态图标块底 / 图标 | `accent_soft` / `accent_text` #3a45ad | `accent_soft` / `accent_text` #aab1ff | `kit::StateKind::NoData` |
| 空态标题 / 说明字号 | `TypeScale::MD` / `SM` | 同左 | `state_block` |
| 空态动作位左缩进 | 38 | 38 | `kit::STATE_ACTION_INSET` |
| 说明最大宽度 | 360 | 360 | `kit::STATE_DETAIL_MAX_W` |
| 局部加载文字 | `TypeScale::XS` + `text_muted` | 同左 | `kit::inline_loading` |
| 任务状态点 / 胶囊 | `Running`: `accent` / `accent_soft`；`Done`: `ok` / `ok_soft`；`Failed`: `danger` / `danger_soft`；`Partial|Blocked`: `warn` / `warn_soft`；`Cancelled`: `text_muted` / `hover` | 同结构换深色值（#3ccf8e / #16352a、#f06d6d / #3d1e20、#f0b44c / #3a2f17 …） | 任务行 `match task.phase` |

主题只决定色板，禁用与空态的几何在四种组合下是同一份（`state_block` + `interaction_style`）。

## 2. 密度

| 项目 | 紧凑 | 标准 | 来源 |
| --- | --- | --- | --- |
| 面板分区间距 `section_gap` | 12 | 16 | `Metrics::section_gap`（面板与四个卡片容器都用它） |
| 页边距 `page_pad`（视口 ≥ 760） | 16 | 20 | `Metrics::page_pad` |
| 页边距 `page_pad`（视口 < 760） | 16 | 16（钳到紧凑档） | `Metrics::for_viewport` / `NARROW_PAGE_WIDTH = 760` |
| 分段控件高度 `control_h` | 26 | 30 | `kit::segmented_sized(.., metrics.control_h)` |
| 局部加载条最小高 `strip_h`（status_h - 4） | 20 | 22 | `kit::inline_loading` |
| 任务行内边距 | `px(14)` / `py(10)` | 同左 | 任务行（不随密度变，行高由字号决定） |

抽屉宽度不随密度变：外壳固定 `min(520, 视口-60)`。

## 3. 由哪些用例钉住

- 四种组合 × 三个宽度：`exchange_states_hold_at_1360`、`_900`、`_640`，每格都断言：就绪时没有空态与加载态、运行中有 `inline-loading`、无表/无结果的空态与主操作、被挡住行动作的可访问名称含原因与"下一步"、任务空态与禁用按钮、动作不越过抽屉边缘。
- 四态判定：`source_state_separates_running_no_table_no_result_and_ready`。
- 原因与下一步：`blocked_actions_name_the_reason_and_the_next_step`。
- 文案差异与英文纯净：`state_copy_differs_per_case_and_english_has_no_chinese`（英文不含 CJK，且"没有表"与"没有结果"不是同一句）。
- 令牌底线：T1 的 `style_literals_follow_design_tokens` 继续扫描 `app/views`，本页没有新增裸字号/裸圆角。

## 4. 截图取得情况

**未取得**原生 浅/深色 × 紧凑/标准 像素截图。原因与替代材料见 `README.md`「证据类型」：本轮无原生窗口与截屏通道授权，未用历史截图替代；交付阶段在最终集成 candidate 上补齐。
