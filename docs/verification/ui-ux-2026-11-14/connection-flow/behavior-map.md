# T3 行为对应：全局连接模态、启动台与按需连接

## Outcome 1 — 启动台、命令搜索、对象树空态与行操作打开同一连接模态；三区、仅主体滚动、字段旁错误、固定页脚

| 入口 | 命令 | 落点 |
| --- | --- | --- |
| 启动台“新建连接” | `conn-new-launcher` → `WorkbenchModel::open_connection_modal(…, Some(ModalControl::LauncherNew), …)` | 同一个模态；记录触发控件用于还焦 |
| 对象树空态“新建连接” | `conn-new-sidebar` → 同上，`ModalControl::SidebarNew` | 同上 |
| 命令搜索 / 菜单 / 快捷键 | `new-connection`（`COMMANDS` 里既有条目）→ `after_shell` → 同上 | 同上 |
| 连接行“编辑 / 复制配置” | `conn-edit` / `conn-duplicate` → `edit_connection(id, duplicate, …)` | 草稿来自那一行，不先保存 |
| 右键行“测试连接” | `conn-test`：模态没开时先 `edit_connection`，再测屏幕上的草稿 | 不再测配置库里的旧值 |

- 草稿、机密、字段错误仍然只有一份（`ConnectionPage`）；`ConnectionModal`（`app/shell/conn_modal.rs`）只持有
  “开着吗、关掉还给谁、这次测试属于哪一代、Tab 停在哪”。因此不存在第二个编辑面。
- 三区：头部（引擎徽标 + 标题 + 关闭）、主体（`#conn-modal-scroll`，`overflow_y_scroll`）、页脚（取消 / 测试 /
  保存 / 保存并连接，`flex_none`）。页面本身不滚动，只有主体滚动。
- 字段旁错误：`Workbench::field_row` / `path_row` 按 `FieldIssue.field` 把文案画在对应输入下方
  （`#conn-issue-<field>`）。无处归属的通用问题仍进主体末尾的提示条，每条只说一次。
- 空态/无命中复用 T1 的 `empty_state_with_action` / `no_match_state_with_action`（带 38px 动作内缩），
  不再往 state block 里 `.child()` 追加裸按钮（T1 P3 advisory 的修法）。

用例：`every_entry_opens_the_same_modal_and_cancel_leaves_no_draft`、`sidebar_connection_menu_opens_the_visible_editor`、
`connections_empty_state_wraps_inside_its_panel_at_every_width`（三档宽度）、`copy_fits_every_width_density_theme_and_locale`。

## Outcome 2 — 首字段自动聚焦、Tab/Shift+Tab 不逃出、Esc/取消还焦触发控件、backdrop 阻断下方控件

| 项 | 实现 |
| --- | --- |
| 自动聚焦 | `ConnectionModal.focus_first_pending`；模态渲染完把焦点交给首个字段（URL）的 `InputState` 焦点句柄。占位符/掩码由 `Workbench::field_hint` 单一来源提供，渲染与聚焦不会写两套文案。 |
| 焦点陷阱 | `overlay_key` 在模态打开时截获 `tab`/`shift+tab` → `modal_step` → `ConnectionModal::step`（按引擎与 TLS/SSH 形状算出的停靠序列成环）；字段用输入框句柄，页脚动作各有 `FocusHandle`。停在页脚动作上按回车等于按下它（`modal_enter`）。 |
| Esc/取消 | `conn-cancel` → `close_connection_modal()`：清草稿与本次机密、不动已保存配置，返回 `(来源区域, 触发控件)`；`render_root` 下一帧把焦点还给启动台/侧栏的“新建”按钮，触发控件不在时退回来源区域。行触发按 S12.3 还给列表容器（行本身不是 Tab 停靠）。 |
| backdrop | `backdrop("conn-modal-backdrop", 0.32, …)` + 面板 `.occlude()`：点击落在遮罩上被吞掉，下方树/编辑器/菜单收不到。 |

用例：`tab_order_stays_inside_the_modal_and_wraps`、`stops_follow_the_shape_of_the_form`、
`closing_returns_focus_once_and_clears_pending_state`、`modal_tab_order_wraps_inside_and_covers_fields_and_footer`、
`every_entry_opens_the_same_modal_and_cancel_leaves_no_draft`。既有测试 `density_theme_and_language_reach_the_rendered_chrome`
现在显式先 `conn-cancel` 再点 `top-search`——这正是“backdrop 阻断下方控件”的可观察形式。

## Outcome 3 — 保存 / 保存并连接 / 测试失败保留 / 取消不保存 / 旧结果隔离；普通保存不强制连接

| 流程 | 行为 |
| --- | --- |
| 保存 | `conn-save`：先 `validate()`，不合法就把错误留在字段旁（`issues` + `notice`），不排队；合法则 `PendingJob::SaveConnection`。成功后 `apply_done` 关模态并刷新列表，`save_and_connect = false`，**不排队任何连接**。 |
| 保存并连接 | `conn-save-connect`：保存成功后用刚拿到的 id 调 `queue_connect(OpenWorkspace)`，只排一次；连接失败时配置已落盘，重试只重连，不重复创建。 |
| 测试 | `conn-test`：校验通过才发请求；`begin_test()` 拿到本次代号，`connections.expect_test(generation)` 让“正在测试”只属于这一代。失败保留 draft + secrets，`issues` 落到字段，模态不关。 |
| 取消 | `close_connection_modal()`：清草稿/机密/字段错误，已保存配置与列表不变。 |
| 旧结果隔离 | `JobRequest.generation` → `JobDone::Test(token, generation, fingerprint, report)` → `ConnectionPage::note_test` 先用代号判定（`generation < test_generation` 直接丢弃，连指示都不碰），再用 `draft_fingerprint` 判定草稿是否还是屏幕上那份。`ConnectionModal::end_test(generation)` 同理只清自己那一代。 |

用例：`plain_save_closes_the_modal_without_connecting`、`save_and_connect_queues_one_open_and_keeps_the_action_for_retry`、
`a_failed_test_keeps_the_draft_and_marks_the_field`、`an_older_test_result_cannot_clear_the_newer_running_indicator`、
`connection_test_runs_on_the_unsaved_draft_and_keeps_it_on_failure`（既有，未回退）、
`stale_connection_test_result_does_not_overwrite_new_edits`（既有，未回退）、
`saving_a_connection_does_not_count_as_a_successful_open`（既有，未回退）。

## Outcome 4 — 启动台：最近/全部连接、紧凑行、新建、SQLite、导入导出；筛选浮层无重叠且支持 Esc / 外点 / 视口碰撞

- 连接页与欢迎区共用 `launcher_header` + `launcher_body`：标题行（标题 + `已显示 / 总数` + 新建 / 打开 SQLite /
  导入 / 导出）与第二行（搜索 + 引擎/环境筛选 + 清除）。
- “最近连接”只认持久化的 `connection_usage` 顺序：`ConnectionPage::recent_ids`（由 `list_rows` 的快照回填）+
  `launcher_recent(6)`；不认识的 id 跳过，绝不用列表前 6 条冒充。
- 紧凑行（`launcher_row`，行高 `Metrics::row_h`）：引擎徽标 / 名称 + 环境标签 / 地址与库 / 状态点 + 状态文字 /
  编辑 / 测试；右键出行菜单；双击走按需连接。
- 筛选浮层从页面内联改成根层浮层（`conn_filter_layer`）：锚点来自按钮这一次真实点击（`conn_filter_at`），
  位置用 `menus::place_popup` 处理右/下边缘翻转，过高时 `max_h` + 自身滚动；点击用 `backdrop` 捕获外点关闭，
  Esc 关闭（`overlay_key`），`model.dismiss()` 也清它。
- 空态/无命中分开：一条连接都没有 → `empty_state_with_action`（新建）；有连接但筛选无命中 →
  `no_match_state_with_action`（清除筛选）。

用例：`recent_rows_follow_the_persisted_order_and_skip_unknown_ids`、`filter_popover_flips_inside_the_viewport`、
`connections_empty_state_wraps_inside_its_panel_at_every_width`、`long_copy_is_truncated_but_fully_available_and_never_holds_neighbours`。

## Outcome 5 — 树展开、连接行双击、打开数据/结构、新建查询统一按需连接，状态文字/指示点与失败重试明确

| 触发 | 命令 | 续接动作 |
| --- | --- | --- |
| 对象树 chevron | `connection_row` 的 chevron：未连接时 `queue_connect(Expand)` | 连上后展开这一行 |
| 连接行双击（启动台/命令搜索） | `connection:<id>` | `OpenWorkspace`：连上后进入工作区并选中该行 |
| 对象树双击 / 打开数据 | `tree-open-data` | `OpenData`（成功后按排队时的对象规格重放） |
| 打开结构 | `open-structure` | `OpenStructure` |
| 新建查询到此对象 | `tree-new-query`；连接行菜单 `conn-new-query` | `NewQuery` / 立即打开绑定查询后排队展开 |

- 统一入口：`Workbench::perform` 最先调 `model.gate_on_demand(id)`。已连接返回 false → 命令照常执行；
  否则 `queue_connect` 记为 `Connecting` + 排队一个 `OpenConnection`（在途时不重复发）+ 写
  `notice_key = ui.conn.connecting` 与连接名。`Connecting` 时复用在途操作，不重复打开。
- 状态文字 + 指示点并存：行上 `status_dot` 旁始终有状态文字与 tooltip（颜色不是唯一载体），对象树行在
  `Connecting` 时显示 `Spinner`。
- 失败保留待办：`fail_pending_intents()` 把动作移到 `connect_retry`，启动台顶部显示红色说明 + “重试”
  （`conn-retry` → `retry_pending_connect()`），重试仍指向同一条连接，不会重复创建配置。
- 安全阀：目标连接不在已载入列表里时 `gate_on_demand` 直接返回 false，不凭空发一次连接。

用例：`on_demand_connect_queues_the_action_and_replays_it_once`、`newly_connected_navigation_is_dispatched_in_the_same_frame`（既有）、
`save_and_connect_queues_one_open_and_keeps_the_action_for_retry`（重试分支）。

## T2 review 的两条 P3

1. **过期结果会收走更新一次的“正在测试”**：`ConnectionPage` 新增 `test_generation` / `test_running_generation` /
   `expect_test(generation)`；`note_test(tasks, token, generation, fingerprint, report)` 先比代号，再比指纹，
   只清自己那一代（`running_is_mine`）。同时 `ConnectionModal::begin_test/end_test` 提供模态侧的同一条规则。
   用例：`a_late_result_never_clears_the_newer_running_indicator`（view 级）、
   `an_older_test_result_cannot_clear_the_newer_running_indicator`（shell 级，走真实任务链）、
   `a_stale_test_result_never_clears_the_newer_indicator`（模态单元）、
   `an_edited_draft_drops_its_own_result_and_only_stops_the_indicator`（同代改字段仍只收指示）。
2. **凭据被拒一律怪用户名**：`field_issue_for_failure` 的 `DatabaseAuthentication` 分支改为
   `conn.password_required → "password"`、`conn.auth_failed`/`conn.redis_auth_failed → "credentials"`（渲染在
   用户名+密码这一组下方）、其余仍为 `username`。用例：`failure_maps_to_the_field_the_user_should_fix`
   （含 Access denied 与 WRONGPASS 两条）。

## 未做（明确留在本任务之外）

- 连接分组、对象树虚拟化（T4）；快速打开的连接/库/表搜索（T5）；历史与设置页收敛（T8）。
- 连接模态没有新增“删除/复制”页脚按钮：删除仍走启动台行菜单 + 既有二次确认，复制配置在行菜单里。
- 没有原生截图与真实输入法/Tab 实机验证（见 `README.md` 的证据类型）。
