# 结果网格流程验证目录（T7）

- 日期：2026-11-14
- 任务：T7「在共享网格中补齐非连续选择、含表头复制、Enter/F2 编辑、双击列宽、自适应整行侧栏和选区批量操作」，
  同时保留三态、密度/主题 token、键盘导航与长值收口。
- 覆盖需求：R3（交互与导航一致）、R4（边界状态与双主题成立）、R5（不引入新依赖与组件库）、R6（表格流程体验）
- 覆盖规范：S7.2（结果网格行高与结果头）、S8.5（加载优先于空、三态语义与下一步）、S9（密度派生尺寸）、S12.4（三态不各自发明一套）
- 改动范围：`src-tauri/src/app/views/grid/{mod,record,session}.rs`、`src-tauri/src/i18n/mod.rs`（只新增/改写本页用到的键）
- 未改动的申报范围：`src-tauri/src/grid/`（驱动与分页层本轮没有需要改的行为，见 `grid-flow-audit.md` 第 8 条）、
  `src-tauri/src/app/shell/`（菜单与命令面板复用既有 `copy-region` / `grid-*` 命令 id，未改外壳代码）

## 计划六条 outcome 与证据

| 计划 outcome | 结论 | 主要证据 |
| --- | --- | --- |
| 非连续选择 + Ctrl+A + 虚拟滚动同一模型，菜单/快捷键/命令面板一致 | 已实现 | `modifier_click_adds_and_removes_single_cells`、`modifier_drag_adds_a_region_and_toggles_it_back_out`、`real_mouse_events_reach_the_cell_gesture_handlers`、`mouse_and_keyboard_produce_the_same_rectangle_and_copies` |
| 单格 Ctrl+C 复制值、多格复制含表头 TSV，多格式与长值保护不回退 | 已实现 | `copy_region_copies_the_value_for_one_cell_and_headers_for_many`、`copy_as_writes_each_format_or_keeps_the_clipboard_with_a_reason` |
| Enter/F2 写编辑、Esc 取消、只读原因与完整值入口、双击分隔柄自适应 | 已实现 | `enter_edits_the_cell_and_ctrl_enter_opens_the_full_value`、`keyboard_editing_uses_a_real_text_input_and_commits_or_cancels`、`double_clicking_a_column_divider_fits_that_column` |
| 记录视图非模态整行字段暂存、选区批量动作、排序/筛选状态、命令路由完整 | 已实现 | `record_panel_stages_a_field_in_place_and_offers_batch_actions`、`record_panel_explains_readonly_fields_instead_of_staging`、`record_view_shows_the_focused_row_and_disables_the_ends` |
| 预览确认、只读边界、批量原子拒绝、失败保留暂存、生产危险二次确认保持 | 未回退 | `grid-flow-audit.md` 第 14 条回归对照表 |
| 1360/900/640 端到端 + 双主题双密度 + 失败证据 | headless 布局级通过，原生像素未取得 | `widths-themes-densities.md`、`grid_flow_runs_at_three_widths_in_both_themes`、`test-receipts.md` 的未取得项 |

## 文件

| 文件 | 内容 |
| --- | --- |
| `grid-flow-audit.md` | 逐条审计：源码入口、修复前的偏差、本次改动、对应验收项与证据（第 10-15 条对应本轮补齐） |
| `widths-themes-densities.md` | 1360/900/640 三档宽度 x 双主题 x 三档密度的实际几何与 token 值 |
| `test-receipts.md` | 实际运行的命令、结果、用例清单与未取得项 |

## 证据类型（请勿混读）

- **源码级**：选区模型（区域 + 减选洞）、复制文本、翻页步长、Tab 逐格移动、单元格语义与内边距派生，
  由 `cargo test` 里的纯函数与会话级用例钉住。
- **headless 布局级**：GPUI 无头测试窗口（1360/900/640 x 720）里真实渲染出的元素存在性、元素边界与真实键击/鼠标事件，
  证明三态换成了底座组件、正文宽度等于窗口宽度、非连续选择能被真实鼠标加选、记录面板字段就地暂存真的写进暂存。
- **未取得：原生像素截图。** 本轮没有原生窗口与截屏通道的使用授权，未生成 PNG。按计划这类证据由 Delivery Owner
  在最终集成 candidate 上采集；本目录不用历史截图替代，也不把 headless 布局结论当成原生像素证据。

## 结论摘要

- 选择模型从「一个矩形」变成「区域表 + 减选洞」：Ctrl/Cmd 单击加减一格、Ctrl/Cmd 拖选加减一块，
  高亮、复制、统计、批量动作和导出读同一份；Shift 扩展、Ctrl+A、行头/列头选择、虚拟滚动仍在同一模型上。
- Ctrl+C 单格给值本身、多格给含表头 TSV；「复制为」各种格式与长值/二进制保护不变。
- Enter 与 F2 都是写入口（只读格给原因并退化为查看，Ctrl/Cmd+Enter 才是显式查看），双击列分隔柄按内容自适应列宽。
- 记录面板可对整行字段就地暂存（非模态，Enter 暂存 / Esc 取消），多格选区时面板底部给出适用批量动作，
  标题旁显示当前排序与筛选状态。
- 三档宽度 x 双主题 x 两档密度下，加选、带表头复制、记录面板与分隔柄自适应全部走通；
  运行中 / 无结果 / 查询错误三态与错误条、键盘焦点环、长值可达性与 NULL 画法没有回退。
