# 历史页与设置页逐条审计（T8）

对照基准：`specs/2026-10-17-desktop-ui-ux-redesign-design.md` S5.2 / S8.2 / S8.3 / S8.5 / S9 / S12；
需求：`docs/requirements/2026-11-14-desktop-ui-ux-unification.md` R1/R3/R4；
方案：`02-technical-solution.md` ADR-1、ADR-10（history 补加载/错误统一状态与条目键盘可达；settings 分区导航、滚动与 640 可达、持久化失败全局可理解反馈）。

历史材料 2026-11-11 审计把这两页记为"符合"，本次按当前 candidate 重新核对：它只覆盖了默认浅色标准与部分宽度，
并且当时两页都已经在用 `kit` 的旧入口。下面每条都从源码入口重新确认。

## 1. 历史页：加载 / 失败 / 无数据 / 无命中四态（S8.5、R4）

源码入口：`src-tauri/src/app/views/history/mod.rs`、`src-tauri/src/app/shell/chrome.rs:history_page`。

| # | 本次之前 | 本次改动 | 验收对应 | 证据 |
| --- | --- | --- | --- | --- |
| 1 | `runs_phase` 与 `saved_phase` 各写一份判断，顺序固定为 空 → 失败 → 无命中；读取中直接落到"无数据"，把"还没读到"画成"没有历史" | 新增 `HistoryPage::phase_for(section)`：失败 → 读取中(空且 loading) → 无数据 → 无命中 → 有行，两个分区共用；`runs_phase`/`saved_phase` 委派给它 | R4 三态语义；outcome 1 | `loading_failure_empty_and_no_match_stay_distinct_for_both_sections` |
| 2 | 读取状态没有模型字段：`enqueue(RefreshHistory)` 之后界面无从知道"正在读" | 新增 `HistoryPage::loading` + `begin_load()`；入队即置位，`apply_list`/`mark_failure` 复位（失败也算结束这次读取） | R4 | 同上（含"读到结果后读取标记必须复位"） |
| 3 | 空态用 `kit::empty_state(...).child(button)` 把裸按钮塞进状态块，主操作与标题不对齐（未用 38px 缩进）；无命中复用同一个空态图标/标题 | 改用 `kit::empty_state_with_action` / `kit::no_match_state_with_action`；无数据主操作是"新建查询"，无命中主操作是"清除筛选"，标题与说明各自成句 | S8.5 / S12.4；outcome 1 | `history_states_are_distinct_and_keep_the_state_action_inset`（headless：主操作相对说明左移 `STATE_ACTION_INSET = 38`，说明宽 ≤ `STATE_DETAIL_MAX_W = 360`） |
| 4 | 失败是自绘的危险色边框卡片（`px(12)``py(10)``Radius::PANEL` + 手拼图标行），只有原因没有下一步；失败与空态还各自成块 | 改为 `kit::error_state_with_action`，带 `history-retry`（`history-refresh`）；失败原因保留在下面，旧行仍可读，不整块替换 | S8.5「读取失败（带重试）」 | 同 `history_states_are_distinct...`（断言 `history-failure` 与重试按钮在位、骨架消失） |
| 5 | 刷新时没有任何"在读取"提示；读到空列表前会先闪一次空态 | 无行时 `kit::skeleton_state(5/4, &Metrics, &Palette)` 首屏骨架；已有行时顶部 `kit::inline_loading`，旧行继续显示 | S12.4 首屏骨架/局部加载分工 | `history_states_are_distinct...`（读取中断言 `skeleton-state` 在位、空态不在位） |

## 2. 历史页：条目键盘可达（S5.2、R3）

| # | 本次之前 | 本次改动 | 验收对应 | 证据 |
| --- | --- | --- | --- | --- |
| 1 | 条目只能用鼠标：点击选中、双击载入、右键出菜单；键盘完全落不到列表 | `WorkbenchModel::history_key(NavKey)`：↑↓ 在**筛选后的可见行**里移动（不跨越被筛掉的记录），Home/End 到首/末行；键盘换行与点选一致地打开预览 | S5.2（焦点留在容器、当前行用选中表达，不把每行变成独立 Tab 停靠）+ outcome 1 | `keyboard_selection_walks_filtered_rows_and_maps_to_the_same_commands`、`history_keyboard_walks_rows_and_enter_only_loads` |
| 2 | 回车没有键盘入口 | 回车载入编辑器，**不执行**：断言 `loads + 1` 且 `executions` 不变，草稿等于该条 SQL | R5「载入不执行」保持不变 | 同上 |
| 3 | Delete 在历史页会命中全局 `mark-delete` 动作，等于把表格的"标记删除"累计到无关页面 | Delete 在历史页转成"删除当前条目"：有键位绑定时由 `perform("mark-delete")` 在视图层分流，没有绑定时由 `overlay_key` 直接落到同一条命令；两种入口都不再累计 `shell.marked_delete` | R3 键盘一致；outcome 1 | `history_keyboard_walks_rows_and_enter_only_loads`（保存的查询先开确认层，两个入口都不动网格指标） |
| 4 | 键盘能力不可发现 | 列表末尾一行 `history-keyboard-hint`：`↑↓` / `Home/End` / `Enter` / `Delete` 键帽 + 现有 `kit::kbd` | R3 | headless 渲染 |

保留项（回归，未改动）：载入只恢复草稿（`plan_from_run/plan_from_saved` 的 `execute: false` 与 `needs_target` 语义）、
窄于 820 的内容区上下堆叠（`history_stacks_preview`）、条目右键菜单、`<200` 行截断、重命名目标钉住。

## 3. 设置页：分区导航、滚动与窄宽可达（S8.3、R6）

源码入口：`src-tauri/src/app/views/settings/mod.rs`、`src-tauri/src/app/shell/chrome.rs:settings_page`。

| # | 本次之前 | 本次改动 | 验收对应 | 证据 |
| --- | --- | --- | --- | --- |
| 1 | 单列长表单：全部四组设置一屏往下排，640 宽要在一条很长的滚动里找 | 两栏：左侧 180 宽分区导航（外观与编辑器 / 数据读取 / 本地数据），右侧只显示当前分区、独立滚动；`settings_side_by_side(content_width) = content_width >= 820`，窄内容区把导航折成顶部一行分段控件 | S8.3 + outcome 2 | `settings_sections_switch_and_nav_stacks_when_narrow`（1360：导航 `w = 180`、在表单左侧；640：导航高 < 60、在表单上方，两者右边界都在窗口内） |
| 2 | 分区只是分组标题，没有导航状态，也没有无障碍节点 | `SettingsSection` 枚举 + `label_key()/command()/parse()`；命令 `settings-section-*` 只切分区、不改设置、不触发写盘；`access_nodes` 增加 `settings-nav` 与三个分区按钮（`selected`/`enabled` 状态） | R3 / S5.6 | `sections_have_stable_commands_and_labels`、`copy_fits_every_width_density_theme_and_locale`（无障碍节点文案都能翻译） |
| 3 | 分组行左右内边距写死，名称最小宽 180，640 宽时控件挤压名称 | 名称最小宽 160、分组行保留 `flex_wrap` + 行间距 16；卡片有 id/role/aria-label（`settings-group-*`） | R6 页面不横向滚动 | `history_and_settings_geometry_holds_in_both_themes_densities_and_three_widths`（三档宽度下卡片右边界都在窗口内） |
| 4 | 版本号跟着表单最后一组排到底部，与"本地数据"无关 | 版本号归到"本地数据"分区，并补一行 `ui.settings.localHint`（设置与历史只存在本机） | S8.3「只存本机」 | 渲染 |

## 4. 设置页：立即生效、重启持久化与失败反馈（R4、R5、outcome 2）

| # | 本次之前 | 本次改动 | 验收对应 | 证据 |
| --- | --- | --- | --- | --- |
| 1 | 主题/密度/语言改动立即生效，但**写盘错误被丢弃**：`persist_if_needed` 里 `let _ = jobs::persist_revision(...)`，界面永远显示"已生效"，用户无法知道重启后会退回旧值 | 新增 `Workbench::persist_settings(cx)`：同一条去重/代次/写盘闸门，但把结果写回 `model.settings_save`（`Saving`/`Saved`/`Failed{detail}`）；`perform` 只对会改本机设置的命令（主题 3 + 密度 3 + 语言 2 + 字号 2 + 编辑器配色 2 + 每页行数 2 + 预览上限 2 + 对象分页 2 + NULL 文本 3 + 超时 3 + 更新开关 1，共 25 个 id）走这条路径，其余命令保持静默写盘 | R4「持久化失败有可理解反馈」；ADR-10 settings 条 | `failed_settings_write_is_visible_then_retry_persists`（状态文件路径被目录占住 → `Failed`；修好后重试 → `Saved` 且 `load_state` 读到当前字号） |
| 2 | 失败只可能出现在设置页，用户离开设置页就彻底看不到 | 设置页表单末尾给出 `kit::error_state_with_action` + `settings-retry-save`；状态栏常驻 `status-settings-save`（危险色 + 图标 + 文案，点击回设置页）——即"全局可理解反馈" | ADR-10「持久化失败有全局可理解反馈」 | 同 `failed_settings_write...`（断言两个入口都在位，成功后同时消失） |
| 3 | 写盘前乐观更新 `last_state`：失败后内容不变的重试会被去重跳过，永远写不进去 | 新路径只在**写成功后**才更新 `last_state`；失败保留旧值，重试必然真的再写一次 | 失败可恢复 | 同 `failed_settings_write...`（重试后状态文件里是该字号） |
| 4 | 主题/密度的即时生效只在模型层被断言，没有渲染级回执 | 新增 headless 断言：同一窗口切浅/深色与紧凑/标准，历史行高 67.5→71.5、设置外观卡片 180→204，主题不改变几何 | S9 / R4 | `history_and_settings_geometry_holds_in_both_themes_densities_and_three_widths` |

保留项（未改动）：合法的设置值归一（非法主题/密度/字号回落到默认）、`SettingsForm::to_settings` 的钳制、
主题/密度/语言立即下发到 QueryPane（`sync_view_settings`）、脏值写盘前再次归一。

## 5. 审计偏差关闭情况

| 页 | 关闭项 | 未关闭项 |
| --- | --- | --- |
| 历史 | 四态混用与读取态缺失、状态块裸按钮、失败无重试、键盘不可达、Delete 误命中网格命令、刷新无提示 | 原生窗口下的键盘走查与截图（环境不可得，见 README） |
| 设置 | 单列长表单缺分区导航、写盘失败被丢弃、失败后无法重试、窄宽卡片挤压、失败反馈只可能出现在本页 | 原生窗口下重启验证与截图（环境不可得，见 README） |
