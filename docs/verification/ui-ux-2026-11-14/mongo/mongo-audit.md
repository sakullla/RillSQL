# Mongo 集合文档页审计与修复（T13）

对象：`src-tauri/src/app/views/mongo/mod.rs`。基线：T1 的 `src-tauri/src/ui/kit.rs`（`state_block` / `empty_state(_with_action)` / `no_match_state(_with_action)` / `error_state(_with_action)` / `inline_loading` / `skeleton_state` / `Metrics` / `Palette` / `interactive`）。本任务只改呈现、交互与状态判定，不改读写、分页、校验与确认语义。

| # | 源码入口（修复前） | 偏差 | 修复结果 | 验证材料 |
| --- | --- | --- | --- | --- |
| 1 | 文档区只有一块：`kit::empty_state` + `filter_error` 红条（旧 1022–1031 行）；状态条裸 `Spinner`（旧 1219 行） | 空集合、筛选无命中、读取失败都落到"没有文档"同一块；读取失败时文档清空但显示成空集合，没有原因也没有重试；读取中只在状态条转圈，内容区没有任何加载语义 | 新增 `DocsState`（NeedTarget / Loading / Error / EmptyCollection / NoMatch / Rows）与 `docs_state`（`mod.rs:360`）；`docs_body` 按态分派（`mod.rs:1325`），`docs_state_block` 给每种态一个下一步（`mod.rs:1421`）：读取中走 `kit::skeleton_state`，读取失败走 `kit::error_state_with_action` + 重试，空集合走 `kit::empty_state_with_action` + 新增文档，无命中走 `kit::no_match_state_with_action` + 清除过滤，没目标走空态 + `Ctrl+B` 键位提示或"重读库与集合" | `docs_states_separate_empty_no_match_and_error`、`error_state_replaces_the_body_and_offers_retry`、`filtered_collection_without_match_offers_clearing_the_filter`、`docs_states_and_cells_hold_at_*` 的六态断言 |
| 2 | 单元格 `div()` 直接 `.on_click`（旧 1050–1075 行），命令 `format!("mongo-cell:{index}:{column_index}")` | 可点单元格没有 hover 底色、按下反馈与焦点环，不是可访问控件（无 role/aria-label）；命令里带的是**列下标**，而 `open_cell` 按**字段名**取片段，点单元格实际打不开值 | 新增 `doc_cell`（`mod.rs:1549`）：`kit::interactive(...).tab_stop(false)` 给 hover / 按下 / 焦点环 / 光标，`role=Button` + `aria_label("{列名} {值}")`；命令改用字段名 `mongo-cell:{row}:{column}`，点击同时把键盘光标同步到该格并聚焦数据区 | `clicking_a_cell_opens_that_column_value`、`docs_states_and_cells_hold_at_*`（点击 → `view_json` 含该列值、`focused_cell` 同步） |
| 3 | 文档表没有任何键盘入口（旧 1022–1090 行） | 整表不可 Tab 进入，也没有方向键/Enter；`kit::interactive` 若逐格使用会产生 20×7 个 Tab 停靠点 | 数据区 `.track_focus(cell_focus)` + `.on_key_down`（`mod.rs:1325`）→ `cell_key`（`mod.rs:388`）：方向键移格、Home/End 到首末列、Enter/空格打开当前格、Esc 先收起只读查看（编辑中的草稿保留）；整表一个停靠点，格子的焦点环按 `focused_cell` 画 | `keyboard_moves_between_cells_and_enter_opens_the_focused_one`、`docs_states_and_cells_hold_at_*` 的方向键/Enter/Esc 断言 |
| 4 | 页面根 `div().id("mongo-pane").size_full().flex().child(left).child(right)`（旧末尾）；面包屑单行不截断（旧 969–979 行）；左栏固定宽度参与 flex 布局（旧 951 行） | 页面根没有 overflow 约束，长库名/集合名会把工具条撑出窗口；窄于 760 展开左栏时挤压文档表而不是覆盖内容区，"页面无横向滚动"与"数据区内部滚动"的职责没写清 | 页面根加 `.relative().min_w(px(0.)).overflow_hidden()`；面包屑 `flex_wrap` + 名称 `max_w(220).truncate()`；窄窗口展开左栏改为绝对定位抽屉 + 遮罩 + `shadow_lg`，点遮罩收回（`mod.rs:1059`–`1080`、`1287`–`1316`）；横向滚动只在数据区 `overflow_x_scroll`（`mod.rs:1360`） | `narrow_pane_keeps_document_columns_readable`（页面根 ≤ 640、数据区 ≤ 640、行宽 ≥ 964）、`narrow_navigation_overlays_instead_of_squeezing_the_table`、`docs_states_and_cells_hold_at_*` 的宽度断言 |
| 5 | 有行时刷新没有任何局部加载（旧 1022 行起）；读取中文案只出现在别处 | "已有内容刷新"与"首屏读取"没有区分：用户看不出刷新正在进行，也不知道旧行还在 | 有行时在数据区顶部插一行 `kit::inline_loading`（`mod.rs:1388`–`1395`），首屏仍用骨架；状态条在没有回执时显示"读取中…" | `docs_states_and_cells_hold_at_*` 的 Loading 态（`skeleton-state` 在、空态不在） |
| 6 | 左栏折叠按钮标签固定取 `mongo.nav.collapse`（旧 966 行） | 按钮文案与当前状态无关，收起后仍写"折叠导航" | 按 `collapsed` 取 `mongo.nav.expand` / `mongo.nav.collapse`（`mod.rs:1106`），新增 i18n 键 | `narrow_navigation_overlays_instead_of_squeezing_the_table`（展开后按钮为"折叠导航"） |
| 7 | JSON 树视图自带一份"空集合/无命中"分支（旧 1275–1281 行） | 两套状态判定会漂移：同一条件下表格与树可能一个说"没有文档"、一个说"没有匹配" | 树视图只负责"有行"的排版，空/加载/错误全部回六态（`mod.rs:1651`） | `docs_states_and_cells_hold_at_*`（切换视图不改变状态判定；状态块由 `docs_body` 统一产出） |

## 保持不回退的边界

| 边界 | 现状 | 验证材料 |
| --- | --- | --- |
| 无效 JSON 不写库、草稿保留、错误留在编辑器 | `quick_validate` 与 `ask_save` 未改；错误仍写入 `editor_error` 并保留 `editor_open` | `columns_follow_fields_and_invalid_json_stays_local`、`live::rendered_controls_keep_invalid_json_and_unconfirmed_delete` |
| 未确认删除 / 未确认写入不落库 | `ask_delete` / `ask_save` 仍只生成确认条，`run_confirm` 才执行 | `live::rendered_controls_keep_invalid_json_and_unconfirmed_delete`（`mongo-delete:0` → `mongo-cancel` 后文档仍在）、`mongo_write_is_previewed_and_target_switch_drops_it` |
| 目标切换丢弃旧预览 | `reset_target_state` 只多清一个 `focused_cell`，其余行为不变 | `mongo_write_is_previewed_and_target_switch_drops_it`、`switching_work_items_drops_structure_admin_redis_and_mongo_state`（`app::shell::tests`） |
| FormatViewer 只读/可编辑 | `ensure_document_viewer` 未改；查看仍是 `ReadOnly`，替换/新增是 `Editable` 且提交只触发 `ask_save` | `live::rendered_controls_keep_invalid_json_and_unconfirmed_delete`（点 `mongo-view:0` 看到文档 JSON） |
| 分页与读取上限 | `PAGE_LIMIT`、`next_skip`、`can_next` 未改 | `columns_follow_fields_and_invalid_json_stays_local`（`next_skip` 断言） |

## 与 T1 底座的关系

- 六态用 `kit` 的入口，没有在页面里另拼"图标 + 标题 + 说明"；主操作放状态块的 `state-action` 动作位（38px 左缩进），不居中另起一行。
- 单元格四态取自 `kit::interaction_style`（hover `p.hover`、按下 `p.active`、焦点环 `p.accent` 2px、可用时不是禁用态），页面没有自己的颜色或焦点环数值。
- 字号只用 `TypeScale`（XS/SM/MD），圆角只用 `Radius`（CONTROL/PANEL），密度经 `Metrics` 读取（行高 `m.row_h`、表头 `m.bar_h`、左栏宽 `m.header_h()*4 + m.pane_pad()`）。
