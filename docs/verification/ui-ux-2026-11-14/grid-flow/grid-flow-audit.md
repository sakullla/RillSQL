# 结果网格流程审计与处置（T7）

对照 R3 / R4 / R5 / R6 与 S7.2 / S8.5 / S9 / S12.4，逐条记录「源码入口 — 偏差 — 修复 — 验证材料」。
探索阶段的记录与 T1 底座（`src-tauri/src/ui/kit.rs` 的 `StateKind` / `state_block` / `empty_state` /
`no_match_state` / `error_state` / `inline_loading` / `skeleton_state` / `Metrics` / `Palette`）作为基线，
本表是当前 candidate 的处置结果。

## 1. 运行中状态：整块转圈，和内容区的结构无关

- 源码入口：`app/views/grid/mod.rs::render_grid_body` 的 `loading` 分支。
- 偏差（修复前）：首屏/换表读取时在内容区正中画一个转圈 + 一行文字；行高、密度与真实表格没有任何关系，
  读完瞬间整块内容跳变成表格（S8.5 要求加载优先于空，且优先骨架/增量渲染）。
- 修复：换成 `kit::skeleton_state(rows, &m, p)`，占位行数取自会话的可视行数
  （`session.viewport_rows().clamp(4, 10)`），占位块高度由 `Metrics::row_h` 派生，
  换密度骨架行高跟着换；骨架是静态色块，没有位移动画，`prefers-reduced-motion` 下同样成立。
- 验证材料：`loading_outranks_emptiness_on_first_open`（加载优先于空、读完才说没数据）、
  `ui::kit::state_tests::skeleton_state_is_density_driven_and_replaces_the_full_screen_spinner`（底座侧）。

## 2. 空态：三种不同的"没有数据"画成同一个空表插图

- 源码入口：`render_grid_body` 的空分支（修复前只按 `GridMode` 换一句标题，正文说明为空串）。
- 偏差（修复前）：筛选没命中、表里没有行、查询没有返回行三种情况共用一张居中的空表插图与空说明，
  没有下一步，也看不出该怎么继续（R4 要求 Carbon 三类语义、文案互不相同、各带下一步）。
- 修复：按 `session.emptiness()` 分派到三个底座入口：
  - 无命中 → `kit::no_match_state_with_action` + 「清除条件」按钮（命令 `grid-filter-clear`，
    同时清掉查找框与筛选条）；
  - 表里没有行 → `kit::empty_state_with_action` + 「插入行」（可写时；只读时只给 `kit::empty_state`）；
  - 查询没有返回行 → `kit::empty_state`，不给写入口（查询结果不是可写来源）。
  说明文案新增三条独立键（`grid.state.noMatchDetail` / `tableEmptyDetail` / `resultEmptyDetail`）。
- 验证材料：`empty_grids_use_the_kit_states_and_their_own_next_step`（三类状态各自出现、彼此互斥、
  按钮文案正确、查询结果没有插入入口）。

## 3. 空表格保留表头骨架

- 源码入口：`render_grid_body` 的第一个分支条件（修复前只要列名非空就画表格）。
- 偏差（修复前）：表里 0 行时仍然画出表头骨架和空表身，屏幕上是一片"像表格的空壳"，而不是空态。
- 修复：只有 `loaded_len() > 0` 才画表格；空表格走第 2 条的三态，内容区被空态整体替换。
- 验证材料：`empty_grids_use_the_kit_states_and_their_own_next_step` 断言此时 `grid-body` 不存在。

## 4. 读失败后原因只出现在状态栏的一行灰字，没有纠正动作

- 源码入口：`app/views/grid/session.rs::state_text`（把失败原因当状态句返回）、
  `mod.rs::render_tools`（把它画成工具栏里一行 260px 截断的灰字）、`render_tools_status`（`notice` 同段再画一次）。
- 偏差（修复前）：失败原因最多出现两次、都是弱化灰色，既不是错误语义也没有「重试」；
  一次都没读到时正文仍然是空表插图而不是错误态。
- 修复：
  - 一次都没读到 → `kit::error_state_with_action`（标题 `grid.state.readErrorTitle`、说明为真实失败原因、
    动作为「重试」`grid-retry`）；查询结果模式没有可重发的读取，只给 `kit::error_state`。
  - 已经读到上一次结果 → 正文原样保留，工具栏下方只挂**一条**错误条 `#grid-read-error`
    （错误色底与下描边、`CircleAlert` 图标、标题 + 截断原因（悬停见全文）+「重试」）。
  - 同一条原因不再在工具栏状态文字与 `notice` 里重复（`read_failure` 存在时不画这两处）。
- 验证材料：`a_first_read_failure_uses_the_kit_error_state_with_retry`、
  `a_failed_read_keeps_the_sheet_and_shows_one_error_entry`（旧结果保留、错误条只出现一次、
  `error-state` 与错误条互斥、清掉原因后错误条收掉）、
  `failed_page_keeps_the_old_page_and_retries_the_same_offset`（旧页、选择、滚动与暂存都保留）。

## 5. 行高、表头、格子内边距与选中底色各自一套常量

- 源码入口：`render_grid_body` 的 `DataTable` 构造、`SheetDelegate::column`、`SheetDelegate::render_td`。
- 偏差（修复前）：行高与表头高虽然取自 `Metrics::row_h`，但格子内边距用的是组件库按 `Size` 推出的固定值
  （4/8），密度切换时只有行高会变；表头文字与选中色散落在各处，看不出唯一的来源。
- 修复：
  - 新增 `row_height(m) = px(m.row_h)`，正文尺寸与表头高度共用；
  - 新增 `cell_padding(m) = { top: 0, bottom: 0, left/right: pane_pad()/2 }`（紧凑 5 / 标准 6 / 宽松 8），
    由 `Column::paddings` 发布，正文格与表头格读同一个值；
  - 选中底色仍是 `p.selection`，表头底色来自 `kit::apply_theme` 写进组件库的 `table_head = p.chrome`。
  网格里没有新增裸尺寸（`ui::kit::token_tests::style_literals_follow_design_tokens` 仍然通过）。
- 验证材料：`grid_geometry_comes_from_tokens_and_stays_inside_three_widths`（三档密度 × 内边距与行高）、
  `density_and_theme_reach_the_rendered_grid`（密度发布会话后内边距随之变化；深浅主题的选中色/表头色）、
  `ui::kit::token_tests::style_literals_follow_design_tokens`。

## 6. 键盘：翻页与 Tab 在网格自持焦点时不移动选择

- 源码入口：`mod.rs` 的 `on_key_down`（只处理方向键与 Home/End）、`session.rs::move_key`。
- 偏差（修复前）：表格自己有 `pagedown`/`pageup`/`tab` 绑定，但焦点在网格本身（点空白、Esc 之后）时
  这几个键什么都不做；`session` 也没有翻页与逐格移动的语义，滚动跟随只在方向键路径里手写了一遍；
  键盘操作后除底色外没有焦点环。
- 修复：
  - `move_key` 增加 `pagedown`/`pageup`（步长 = 会话可视行数），滚动跟随抽成 `follow_focus()`；
  - 新增 `move_tab(forward) -> bool`：在可见列之间逐格走、行尾换行，边界处返回 `false`；
  - 网格自己的 key context 绑定 `tab`/`shift-tab` 到 `GridNextCell`/`GridPrevCell`，
    `step_cell` 在边界处 `cx.propagate()`，把 Tab 交还外壳的焦点遍历（键盘用户不会被关在网格里）；
  - 表格量出的可见行数（`visible_rows_changed`）由网格每帧发布给会话，翻页步长与可见窗口因此和真实窗口一致；
  - 键盘焦点环：`focus_inside && window.last_input_was_keyboard()` 时把焦点格发布给 delegate，
    该格画 2px 强调色描边（`kit::FOCUS_RING_W` 的语义宽度），鼠标点选不画。
- 验证材料：`paging_and_tab_move_the_focus_and_keep_the_scroll_following_it`（会话级：翻页、Tab 换行、
  隐藏列跳过、边界返回 false、滚动跟随）、`page_and_tab_keys_move_the_selection_and_paint_the_focus_ring`
  （渲染级：真实的 `pagedown`/`tab`/`pageup` 键击改变选区、焦点环跟到新格子、末格不再移动）、
  `real_mouse_events_reach_the_cell_gesture_handlers` 等既有用例（鼠标路径不回退）。

## 7. 单元格语义靠显示文本猜；长值只有截断没有出处

- 源码入口：`SheetDelegate::render_td`（修复前 `text == "NULL" || text == i18n null`）、
  `session.rs::cell_label` / `classify_with_limit`（已有 `CellKind`，但界面没用）。
- 偏差（修复前）：真的存着字符串 `"NULL"` 的格子会被画成空值（弱化斜体），与数据库 NULL 混淆；
  长文本只有视觉截断，格子上没有任何取得完整值的入口说明。
- 修复：新增 `session.cell_kind_at(row, column) -> CellKind`（与 `cell_label` 同一套判据：
  先看被拦下的暂存值，再看原值/预览信封），渲染据此决定画法；新增 `cell_tooltip(locale, kind, text)`：
  普通文本给整段原文，大值/二进制给「按 Ctrl/⌘+Enter 查看完整值」，NULL/空字符串不给提示。
- 验证材料：`cell_kind_separates_null_empty_and_the_text_null`（会话级）、
  `long_values_truncate_with_the_value_reachable_and_null_differs_from_empty`（渲染级：三种语义、
  暂存格的标记、长文本的整段提示）。

## 8. 横向滚动可能把整个页面撑宽（申报范围内无独立改动）

- 源码入口：`render_grid_body` 的正文容器。
- 偏差：正文容器只写了 `flex_1().min_h(0)`，列宽总和大于窗口时没有约束横向溢出的地方，
  查询页/表数据页的横向滚动条可能被网格列宽带出来。
- 修复：正文容器补 `min_w(0)` 与 `overflow_hidden`，横向滚动只发生在表格自己的滚动容器里。
- 验证材料：`grid_geometry_comes_from_tokens_and_stays_inside_three_widths`——六列各 260px（合计 1560px）时，
  640/900/1360 三档窗口下正文宽度实测等于 640/900/1360。
- 关于 `src-tauri/src/grid/`：分页、SQL 生成、导出与单元格编码在本轮没有需要改的行为；
  失败保留、NULL 与文本 NULL 的编码差异已有既有用例（`sqlite_pages_keep_null_empty_binary_large_insert_and_delete_apart`、
  `where_copies_keep_null_and_staged_values_distinguishable`）钉住，本轮未改该目录。

## 9. 未处理（明确不在本任务范围）

- 查询页结果区外层的页签条、结果头与消息页仍由 `app/views/query/` 负责；
  本任务只保证网格自身的三态与错误条，不改查询页布局。
- 原生像素截图与真实数据库连接下的三档宽度走查未取得，见 `test-receipts.md`。

## 10. 非连续选择：Ctrl/Cmd 只能把矩形扩大，不能加第二块区域

- 源码入口：`session.rs` 的 `anchor/focus/selected` 三元组、`selection_bounds()`、`selection()`、
  `selection_count()`、`mod.rs::cell_press/cell_drag`、`cell_selected`、`SheetDelegate::selected`。
- 偏差（修复前）：选择只能是「锚点到焦点」的一个矩形。Ctrl/Cmd 单击把焦点挪到别处，`anchor` 留在原格，
  于是选择变成两格之间的大矩形——中间没点过的格子被一起选上，复制和统计都多算。
  `modifier_click_extends_the_existing_selection_into_a_rectangle` 这条用例当时断言的正是这个错误语义
  （两次加选 = 2x2 矩形、`selection_count() == 4`）。
- 修复：
  - 会话里用一张区域表 `regions: Vec<Rect>` 加一张减选表 `minus: Vec<Rect>` 表达选择：
    普通单击/拖选/行头/列头/Ctrl+A 都写成一块矩形；Ctrl/Cmd 单击**追加**一格，再点同一格把它推进 `minus`。
  - `toggle_press` / `toggle_drag` / `finish_region` 负责加选与减选：按下时就确定这一拖是加还是减
    （看按下格当时是否已选中），拖动把这一块扩成矩形，松开才落定；整块被挖空的区域在松开时收掉。
  - `selection()` 变成区域并集减洞（`BTreeSet` 去重，按行优先排序），`selection_count()` 数并集格子数；
    `selection_regions()` / `selection_holes()` 交给绘制层逐格判断，`cell_selected` 因此按「区域 + 洞」算，
    行号列只在某一区域横跨整行时点亮。
  - `selection_bounds()` 保留原语义（包围盒），只用于整行/整列这类摘要判断；复制、导出、批量动作和统计都不再读它。
  - 排序、筛选、翻页、隐藏列之后区域里的显示行号已经换人，无法按行身份还原：`reattach_selection` 收拢成主矩形
    （`regions.truncate(1)` + 主矩形按行 id 重定位），这是写进注释的明确行为。
  - 修掉那条错误用例：换成 `modifier_click_adds_and_removes_single_cells`（三格独立、再点减一格）
    与 `modifier_drag_adds_a_region_and_toggles_it_back_out`（拖出一块区域、再拖一遍挖掉）。
  - 表格事件回声治理：`sync_table` 写回焦点时把格子记进 `echo_cells`，`apply_table_event` 认得出自己发出的
    `SelectCell` 就忽略。没有这一层，一次选择更新连发的旧回声会把刚加进来的区域冲掉
    （`grid_flow_runs_at_three_widths_in_both_themes` 里先复现、再修好）。
- 一致性：Shift 仍扩展最后一块矩形并撤掉减选；Ctrl/Cmd+A 收成整表一块；单元格右键菜单的
  「复制选区（单格为值，多格含表头）」与 `Mod+C` 的快捷键帮助指向同一条命令 `copy-region`。
- 验证材料：`modifier_click_adds_and_removes_single_cells`、`modifier_drag_adds_a_region_and_toggles_it_back_out`、
  `real_mouse_events_reach_the_cell_gesture_handlers`（真实鼠标事件下的加选）、
  `mouse_and_keyboard_produce_the_same_rectangle_and_copies`、`plain_click_replaces_an_existing_rectangle`。

## 11. 复制：Ctrl+C 只给字段，表头要去另一条命令拿

- 源码入口：`command("copy-region")` -> `session.tsv()` -> `tsv_grid(points)`。
- 偏差（修复前）：Ctrl+C 复制的是纯字段块，粘到别处不知道列名；含表头的文本只存在于「复制为 -> TSV」
  这条独立命令里，两条命令给出不同结果，用户得自己记住该用哪一条。
- 修复：新增 `session.copy_text()`——单格复制值本身，多格复制「列名一行 + 每行值」；
  `command("copy-region")` 改用它。「复制为」的其它格式（CSV / JSON / Markdown / INSERT / SQL 字面量）
  以及长值、二进制的原值保护完全不动（`copy_as` 仍走同一套读原值 + `export::render`）。
- 对齐：`tsv_grid` 按整段列范围补齐空字段，非连续选择复制出来的块是矩形的、和表头一一对齐。
- 验证材料：`copy_region_copies_the_value_for_one_cell_and_headers_for_many`（单格 `甲`；2x2 多格
  `id<TAB>name / 1<TAB>甲 / 2<TAB>乙`；`grid-copy-as-tsv` 同一份；`grid-copy-as-csv` 仍是 CSV 自己的引号与 CRLF；
  `copy-sql-value` 仍是 SQL 字面量）、`copy_as_writes_each_format_or_keeps_the_clipboard_with_a_reason`（不变）、
  `modifier_click_adds_and_removes_single_cells`（非连续块 `1<TAB> / 2<TAB>乙`，带表头 `id<TAB>name` 一行）。

## 12. Enter 打开查看面板而不是进编辑；双击分隔柄自适应是否真的走通

- 源码入口：`mod.rs` key handler 的 `key == "enter"`、`grid_shortcut`、`CELL_MENU`、`SheetDelegate::fit_column`。
- 偏差（修复前）：快捷键帮助登记的是 `edit-cell = "Enter / F2"`，但 Enter 实际执行 `grid-value-open`——
  帮助和实现互相矛盾，用户按 Enter 期待编辑却弹出查看面板。
- 修复：Enter 走 `command("edit-cell")`（可写格进原位编辑；只读格按 `write_reason` 退化成查看并说明原因），
  `Ctrl/Cmd+Enter` 才是显式的 `grid-value-open`；`grid.value.openFullHint` 同步改成「按 Ctrl/⌘+Enter 查看完整值」，
  大值/二进制格的悬停提示因此不再说谎。
- 分隔柄：双击的等价入口是 `TableState::fit_col` -> `SheetDelegate::fit_column`（量列名与已加载行的最宽值）
  -> `TableEvent::ColumnWidthsChanged` -> `session.set_column_width`。本轮补了直接走 `fit_col` 的用例，
  并确认行号列（没有可量内容）返回 false、不参与自适应。
- 验证材料：`enter_edits_the_cell_and_ctrl_enter_opens_the_full_value`（Enter 进编辑并带出原值、Esc 取消、
  Ctrl/Cmd+Enter 打开查看）、`keyboard_editing_uses_a_real_text_input_and_commits_or_cancels`（F2/Esc 不回退）、
  `double_clicking_a_column_divider_fits_that_column`、`auto_fit_widens_a_long_text_column_within_the_limit`
  （列宽上限 640 仍生效）、`lazy_delegate_survives_owner_release_and_direct_header_fit`。

## 13. 记录视图只读：整行字段不能就地暂存

- 源码入口：`app/views/grid/record.rs::render_record`、`mod.rs::open_record_value`。
- 偏差（修复前）：记录面板每个字段只有一个「查看完整值」按钮，想看别的列要回网格里找格子；
  没有任何整行字段暂存入口，计划要求的「非模态整行字段暂存」缺位。
- 修复：
  - 字段值本身可点：可写时值就地变成 `InputState`（非模态，不弹窗），Enter 交给 `finish_record_edit`
    走与网格原位编辑同一个 `stage_text` 门控与校验，Esc 交给 `cancel_record_edit` 丢弃这次输入；
    只读时同一处给 `write_reason` 并退化成查看完整值。
  - 暂存失败仍保留已有暂存（只多一句原因），预览 -> 确认 -> 提交链路一点没改。
  - 选区多于一格时，面板底部出现适用于该选区的批量动作条（设值 / 设为 NULL / 复制选区）；
    大选区只显示「多格」，不为了一个数字把整张表展开成格子。
  - 面板标题旁挂排序/筛选状态 pill（`sort_text()` + 已应用条件条数），逐行看数据时也知道结果被什么条件筛过。
- 验证材料：`record_panel_stages_a_field_in_place_and_offers_batch_actions`（Enter 暂存、Esc 取消、
  已有暂存保留、多格选区出现 `#grid-record-batch`）、`record_panel_explains_readonly_fields_instead_of_staging`
  （只读不进暂存、给原因、退化查看）、`record_view_shows_the_focused_row_and_disables_the_ends`、
  `record_toggle_button_opens_and_closes_the_record_view`。

## 13b. 复审补修（T7-A3）：预览信封与换行后的暂存状态

第一节的修复只覆盖了记录面板"点开就编辑"，漏了两处状态真实性的边界。

- 源码入口：`mod.rs::record_field_press`、`mod.rs::begin_record_edit`（对照 `mod.rs::begin_edit` 的原值门控）、
  `mod.rs` 的 `grid-record-next` / `grid-record-prev` / `grid-record-close` / `grid-record-toggle` 分支、
  `record.rs::render_record`。
- 偏差（补修前）：
  - 长文本/二进制格在记录面板里点开时没有 `begin_edit` 那道门控，`edit_text` 因为 `original()` 为空而给出空串，
    字段打开就是空输入框；Enter 随后把 `Some("")` 暂存成一次 UPDATE，等于静默改写用户没见过的值。
  - `RecordField.preview` 算出来了却没画，截断显示和完整值在面板上长得一样。
  - 就地暂存开着时切上一行/下一行/关面板都不清 `record_edit` / `pending_record`：下一行显示上一行的文字，
    Enter 还会把它写进错的那一行；`record_edit.is_some()` 期间网格按键也被一并吞掉。
- 修复：
  - `record_field_press` 复用 `begin_edit` 的判据（`needs_full` 且没有缓存原值时先 `begin_full_cells` +
    `start_full_reads`，pending id 为 `record-edit`），读到原值后 `command("record-edit")` 回到同一条入口再进就地暂存；
    `reads` 为空时用 `cell_block` 说明原因，不进入暂存。`begin_record_edit` 再做一次同样的判断兜底
    （按下之后这一行被重读的情况），绝不摆出空输入框。
  - 字段行右侧按 `field.preview` 画 `grid.record.preview`（预览）pill，悬停给 `grid.value.previewTag`。
  - `discard_record_edit()` 统一丢掉 `record_edit` / `pending_record` / `record_edit_deferred`，
    换行、关面板、换表（`show_query` / `open_table` / `clear_sheet`）都先走它；换行与关闭同时把焦点交回网格。
- 验证材料：`record_panel_reads_the_original_before_staging_a_long_text_field`（原值没到手不开编辑、给原因；
  原值回来后输入框是原值，Enter 暂存的是原值而不是空串——用生成的 UPDATE 反查）、
  `record_panel_never_opens_a_blank_editor_for_a_preview_cell`（绕过入口直接摆出暂存状态也不会变成空输入框、
  Enter 不写空串）、`record_panel_switching_rows_discards_the_in_place_edit`（换行丢弃、不写错行、关面板丢弃、
  重开不带上次文字）、`record_panel_marks_preview_fields`（预览字段有标记、普通值没有）。
  三个回归用例在去掉对应修复后会失败（已实测），不是只跑一次通过。

## 13c. 复审补修（T7-A4）：就地暂存改成按行身份绑定，失效收敛到一个钩子

13b 的补修只把作废挂在了换行/关闭/换表五处，根因没动：`record_edit` 仍是一个列号，
`finish_record_edit` 在 Enter 那一刻用当时的焦点行重新推导目标行（`stage_text(row, ...)`）。
凡是改变当前行、又没走 next/prev/close 的路径（鼠标点格、表格选择事件、行头选择、翻页、
排序/筛选后重读、`set_preview_kb` 重读）都会留下一个活着的暂存：新行渲染成上一行的文字，
Enter 就把它——或清空后的空串——写进用户没看过的那一格。

- 源码入口：`mod.rs` 的 `record_edit` / `pending_record` / `record_edit_deferred` 字段类型、
  `record_field_press`、`begin_record_edit`、`finish_record_edit`、`cancel_record_edit`、
  `discard_record_edit` / `record_row_changed`，以及渲染处 `record::RecordEdit { column }`；
  `session.rs` 新增 `raw_value_of` / `edit_text_of`（按稳定行 id 取原值）。
- 修复（对齐网格原位编辑的 `PendingEdit`：row_id + `stage_text_by_id`）：
  1. 目标格在按下那一刻就绑定身份：三个字段都改成 `(行 id, 逻辑列)`，行 id 是唯一键。
  2. `finish_record_edit` 只走 `stage_text_by_id(row_id, column, ...)`，不再按焦点行重新推导；
     目标行已经离开当前快照时直接收掉并说明原因，一个字都不写。
  3. 失效收敛到一个钩子 `record_row_changed()`：鼠标点格、表格 `SelectCell`/`RightClickedCell`、
     `SelectColumn`、`SelectRow`/`DoubleClickedRow`、双击进编辑、翻页、页大小变化、
     排序/筛选重读（`reload_after_condition_change` 顶部）、整页替换重读、`set_preview_kb`、
     换表（`show_query`/`open_table`/`clear_sheet`）、记录面板换行/关闭/关闭开关、跳到已暂存格，
     全部调它，不再各处分头清字段。
  4. `finish_record_edit` 复用 `begin_edit` 的 `needs_full && full_text_of().is_none()` 判据兜底：
     值还是预览信封时绝不写，空串不能顶替用户没看过的长文本/二进制原值。
  5. 渲染也按身份判：只有记录面板正在显示的那一行等于暂存目标行时才摆输入框，
     行换了就先不显示，不会把上一行的文字画成这一行的值。
- 验证材料：`record_panel_mouse_row_change_discards_the_in_place_edit`（点另一行后 Enter 不写进新行）、
  `record_panel_sort_change_discards_the_in_place_edit`（排序改变行集合后作废，Enter 不写任何行）、
  `record_panel_enter_never_writes_a_blank_over_an_unread_preview_value`（原值未读到的预览格，
  空输入框按 Enter 也不写空串）。三个用例都做过反向核对：去掉对应的钩子或判据后各自失败，恢复后通过。

## 14. 保持不变的行为（回归对照）

| 行为 | 用例 |
| --- | --- |
| 预览确认后才写库、取消不写 | `sqlite_preview_confirm_writes_and_cancel_or_reject_does_not` |
| 只读边界（查询结果 / 未定位 / 非表结构各自原因） | `readonly_reason_is_painted_for_every_specific_denial`、`readonly_results_keep_copy_and_explain_the_reason` |
| 批量写入原子拒绝（任何一格不合格整批不生效） | `ctrl_v_pastes_tsv_only_from_the_grid_focus_and_respects_readonly`、`set_value_on_selection_stages_one_batch_from_the_edit_bar` |
| 失败后保留暂存与旧页 | `failed_page_keeps_the_old_page_and_retries_the_same_offset`、`a_failed_read_keeps_the_sheet_and_shows_one_error_entry` |
| 长值 / 二进制原值保护 | `viewer_shares_one_original_with_copy_edit_where_and_export`、`failed_full_value_marks_the_viewer_error_and_retries_without_the_preview` |
| 生产危险操作二次确认 | 由执行确认链路负责（本任务未触碰）；网格只生成预览 SQL |

## 15. 交付完整性

- 本轮渲染的 7 个文案键（`grid.state.readErrorTitle` / `grid.state.clearConditions` / `grid.state.noMatchDetail` /
  `grid.state.tableEmptyDetail` / `grid.state.resultEmptyDetail` / `grid.action.retry` / `grid.value.openFullHint`）
  连同记录面板与复制的新键一起进了本次改动；`every_grid_state_key_exists_in_the_catalog` 直接查
  `i18n::catalog_keys()`，不再靠 `text()` 回退成键名。
- 验证文档从 `docs/verification/ui-ux-2026-11-14/result-grid/` 移到计划里写的
  `docs/verification/ui-ux-2026-11-14/grid-flow/`。
