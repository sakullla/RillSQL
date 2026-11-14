# 行级交互：长名称、箭头、状态、三态、键盘与右键

## 逐项对照

| 项目 | 实现 | 证据 |
| --- | --- | --- |
| 长名称 | 连接行/树行/分组标题都 `truncate()` + `tooltip_text(完整名称)`，无障碍名称仍是完整名称 | `explorer_rows_keep_long_names_status_chevrons_and_object_actions`：原名重复 3 次（48 字）的连接行 label 含完整名称，行宽 ≤ 侧栏宽 |
| 展开箭头 | 连接行 `nav-chevron-<id>`、分组标题 `group-chevron-<id>`、树行 `nav-chevron-<节点 id>`；叶节点保留 12px 占位 | 同上的两个 `try_find(...).is_some()` 与 `groups_and_collapse`、`group_row_menu...` 用例 |
| 状态 | 连接行仍用 `LinkStatus` 四态：状态点 + 颜色 + 文字（`aria_description` 为“完整名称 · 状态文案”，tooltip 同义） | 同上：断言 `description` 含 `connections.status.connected` 的本地化文案 |
| 空库 | 已连接但无对象 → 说明行 `Note{ key: ui.explorer.noObjects }`；加载中 → `catalog.tree.loading`；目录错误 → `ui.explorer.error`（不再画成“没有对象”） | `empty_databases_and_connection_errors_have_their_own_explorer_rows`；`nav_empty_key` 既有断言 |
| 无命中 | 筛选后没有任何命中 → 单条 `NoMatch` 行（不是空态、不是错误） | `filtering_to_nothing_shows_one_clear_no_match_row` |
| 错误 | 连接级读取失败 → `ConnectionError` 行（原因 + 重试按钮，动作 `nav-retry:<连接节点 id>`）；目录整体失败 → 顶部一条 `CatalogError` | `empty_databases_and_connection_errors_have_their_own_explorer_rows` 断言 `catalog-error` 位于首行、`ConnectionError` 带原文原因 |
| 键盘导航 | Up/Down/Home/End 在“分组标题 + 连接行 + 树行”同一条序列里走；Right 展开、Left 折叠/回上一行；Enter：分组折叠、连接按需连接并展开、对象打开（沿用既有规则） | `explorer_keyboard_walks_groups_connections_and_tree_rows`；`keyboard_navigation_expands_moves_and_collapses_without_running_sql`（既有，回归通过） |
| 对象右键 | 右键把 `nav_menu_node` 钉在被点的对象上，菜单动作（打开数据/结构、DDL、模板、危险操作）都用这一行的节点 id | `explorer_rows_keep_long_names_status_chevrons_and_object_actions` 断言 `nav_menu_node == 对象 id`、菜单含 `tree-open-data`/`open-structure`；既有 `tree_actions_bind_to_the_row_and_routines_never_open_a_data_page` 回归通过 |
| 拖拽归组 | 连接行 `on_drag(ConnectionDrag)`，分组标题 `drag_over` 高亮 + `on_drop` 发 `group-assign` | `dragging_a_connection_onto_a_group_row_queues_the_same_assign_command`：真实 `window.drag_to` 后 `pending_group_op == Assign{pg, g1}` |

## 键盘的状态一致性

- 模型只保留**一个**当前行：`explorer_current`（分组/连接/树节点三选一）。当树焦点被别的入口（跳转定义、鼠标点行）改动后，`explorer_current_row()` 以树焦点为准并丢弃过期记录，屏幕上的选中行与键盘不会说两套话。
- 键盘移动把目标行写进 `reveal_row`，视图下一帧按行高前缀和算出偏移并 `set_offset`，把该行滚进视口；鼠标点行本来就在视口内，不触发滚动。
- 键盘与鼠标都只发命令字符串（`group-toggle:<id>`、`nav-toggle:<节点 id>`、`nav-focus:<节点 id>` 等），没有给虚拟化后的行留下下标依赖。

## 未解决事实

- **无原生像素证据**：长名称截断后的实际像素、tooltip 出现位置、分组拖拽高亮在真机上的手感都没有截图；本轮只给出 headless 边界与断言。
- **多库/多模式端到端未跑**：PostgreSQL/MySQL 的“库/模式/对象按层展开”需要 docker fixture。当前实现的读取分层是：连接建立时读库清单（PG/SQL Server 另附当前库的 schema 清单）；**会话当前库以外的库现在是可展开的库行**，展开时按库读一次（PostgreSQL 另开临时会话，SQL Server 用三段名），先显示加载态再出现模式/分组/对象，见 `database-lazy-loading.md`。端到端仍只在 SQLite 真实会话上验证，PostgreSQL/SQL Server 只有模型级与源码级断言。
- **分组输入面板不自动聚焦**：打开后需要点一下输入框或 Tab 过去；Esc 取消、保存按钮与命令都可用。
