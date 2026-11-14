# 关系图页审计与修复（T10）

对象：`src-tauri/src/app/views/diagram/mod.rs`。基线：T1 的 `src-tauri/src/ui/kit.rs`（三态入口、`interactive` 四态、`Metrics`/`Palette`）。本任务只改呈现、交互与状态判定，不改读取、布局算法（`layout_columns`）、连线绘制与打开对象的语义。

| # | 源码入口（修复前） | 偏差 | 修复结果 | 验证材料 |
| --- | --- | --- | --- | --- |
| 1 | 卡片 `div()` 只有 `.on_hover`/`.on_click`（旧 1085–1170 行） | 可点卡片没有 hover 底色、按下反馈、键盘 focus-visible 焦点环，也不是 Tab 停靠点；键盘完全进不到卡片 | 卡片链尾接 `kit::interactive(card, &p, false).opacity(opacity)`：hover/active 底色、2px 强调色焦点环、Tab 停靠点；`opacity` 放在底座之后，悬停时其余卡片的 dim 变淡保持不变（`mod.rs:1706`） | `card_keyboard_equivalents_match_the_mouse_gestures`（四态取值来自 `kit::interaction_style`）、`diagram_four_states_and_keyboard_hold_at_*`（卡片元素存在且 `pane-host` 内可 Tab 到达） |
| 2 | 卡片右键菜单只能靠鼠标第二键；无关闭按钮，Esc 无处处理 | 键盘无法打开/关闭卡片菜单；鼠标打开的菜单没有显式关闭入口 | 卡片加 `.on_key_down` → `card_key`：Enter 打开数据、Shift+Enter 看结构、菜单键/Shift+F10 打开菜单、Esc 收起；菜单条加关闭按钮（`diagram-menu-close`）、`Esc` 键位提示，并在页面根节点补 `pane_key` 兜底（`mod.rs:352`、`1685`、`1734`、`1801`）；A2 之后又补齐鼠标侧的关闭路径：页面根节点的 `on_mouse_down` 收起菜单，菜单条自己停掉按下事件的传播，所以点菜单里的按钮不会被这条路径抢先拆掉 | `card_keyboard_equivalents_match_the_mouse_gestures`、`pane_keys_zoom_and_dismiss_without_touching_the_diagram` |
| 3 | `open_spec` 解析失败时 `self.error = diagram.needTarget`（旧 136 行） | “还没选目标”被当成“读取失败”：显示危险色错误块，重试按钮又被 `connection_id` 为空挡掉，页面只剩一句红字，没有下一步 | 新增 `PaneState`（Loading/Error/NeedTarget/Empty/Cards）统一判定；无目标走 `kit::empty_state_with_action`，说明“先选择已连接的库或 schema”，动作位给出 `Ctrl+B` 打开资源管理器的键位（`mod.rs:334`、`1305`、`1774`） | `four_states_carry_the_matching_next_step`；`diagram_four_states_and_keyboard_hold_at_*` 断言 `empty-state` 存在、`error-state` 不存在、`state-action` 存在 |
| 4 | 空图分支 `kit::empty_state(...)` + `notice.unwrap_or_default()`（旧 872 行） | 空态没有下一步；没有 notice 时说明是空字符串，用户不知道为什么会空 | 改 `kit::empty_state_with_action`：说明优先用读取期 notice（例如“PostgreSQL 关系图需要一个 schema。”），否则用新增 `diagram.emptyDetail`；动作位为“重新读取”（`diagram-read-again` → 既有 `diagram-retry` 通路）（`mod.rs:1352`） | `four_states_carry_the_matching_next_step`；`diagram_four_states_and_keyboard_hold_at_*` 断言 `empty-state`+`state-action`+`diagram-read-again` |
| 5 | 错误分支把 `kit::error_state` 居中，再用 `.when(...)` 追加裸按钮（旧 855–871 行） | 重试按钮被追加成独立子元素，丢掉底座 38px 动作缩进（T1 记录的 P3 提醒），和空态排版不一致 | 改 `kit::error_state_with_action`：重试进状态块动作位，与标题文字左对齐；无连接时退回不带动作的 `kit::error_state`（`mod.rs:1330`–`1345`） | `four_states_carry_the_matching_next_step`；`diagram_four_states_and_keyboard_hold_at_*` 断言 `error-state`+`error-state-detail`+`diagram-retry` |
| 6 | 搜索框只改 `highlight`/`center_pending` | 搜不到任何表时没有任何反馈，用户无法区分“没有命中”和“没有表” | 画布上方加无命中提示条（`diagram-no-match`：图标 + “没有匹配的表” + “清除搜索”按钮 `diagram-search-clear`）；画布保持可见，清空即恢复现场（`mod.rs:1363`–`1392`、`command` 的 `diagram-search-clear` 在 `169` 行） | `four_states_carry_the_matching_next_step`（`search-no-match` 行）；`diagram_four_states_and_keyboard_hold_at_*` 断言提示条存在、画布仍在、点“清除搜索”后模型条件为空**且输入控件里的文字为空**（`search_control_text`，A2 补齐） |
| 7 | 缩放只有工具栏按钮与滚轮 | 键盘用户只能 Tab 到按钮；`+`/`-`/`0` 这类通用缩放键不生效 | 页面根节点 `on_key_down` → `pane_key`：`+`/`=` 放大、`-` 缩小、`0` 复位、Esc 收起菜单与悬停；搜索框聚焦时整体让位（`mod.rs:379`、`1801`） | `pane_keys_zoom_and_dismiss_without_touching_the_diagram` |
| 8 | 页头把标题、180px 搜索框、5 个按钮塞在单行（`page_header` 追加） | 640 宽时工具条被挤出内容区，右侧按钮点不到 | `viewport_size() < kit::NARROW_PAGE_WIDTH` 时页头换成两行：标题行 + `diagram-tools` 行（搜索框 `flex_1` 占满剩余、缩放/适应/返回在右）；页边距用 `Metrics::for_viewport` 钳到紧凑档（`mod.rs:1252`–`1302`） | `diagram_four_states_and_keyboard_hold_at_640` 断言 `diagram-tools` 存在且放大按钮可用；1360/900 断言不存在（单行） |
| 9 | 无关内容的保护 | — | 只读：不改 `src-tauri/src/diagram/`、不新增 SQL、不写库。悬停关联（hot/linked/dim）、连线箭头、双击打开对象、只读边界全部保持；`lines()` 的既有行（`column/pk/type/outside/wire`）未删除，只新增 `menu`、`search-match`、`search-no-match` 三行观测 | `sqlite_diagram_shows_cards_wires_hover_and_does_not_write`（读取前后 `sqlite_master` 与 `orders` 行数一致、双击后出现打开请求） |

## 状态判定（唯一的五种呈现）

| 状态 | 判定 | 呈现 | 下一步 |
| --- | --- | --- | --- |
| Loading | `loading` 为真 | `kit::loading_state`（整块一次性读取） | 读完后按结果落到 Empty/Cards/Error |
| Error | `error` 非空 | `kit::error_state(_with_action)`，说明里带“目标：原因” | 重试（`diagram-retry`） |
| NeedTarget | 连接为空，且不在读取/失败 | `kit::empty_state_with_action`（图标 `ChartNetwork`） | `Ctrl+B` 打开资源管理器选库/schema |
| Empty | 有连接但读到的表为空 | `kit::empty_state_with_action` | 重新读取（`diagram-read-again`） |
| Cards | 有表 | 画布（卡片 + 连线 + 悬停 + 卡片菜单） | 卡片上的四态与键盘等价操作 |

## 键盘等价映射

| 鼠标 | 键盘 | 入口 |
| --- | --- | --- |
| 双击卡片 = 打开表数据 | `Enter` | `card_key` → `request_open(table, false)` |
| （菜单里的“查看表结构”） | `Shift+Enter` | `card_key` → `request_open(table, true)` |
| 右键卡片 = 卡片菜单 | 菜单键 / `Shift+F10` | `card_key` → `open_card_menu` |
| 点菜单外/Esc 关菜单 | `Esc`（卡片或菜单内任意元素聚焦）；鼠标点页面其它位置同样收起 | `card_key`/`pane_key`，以及页面根 `diagram-pane` 的 `on_mouse_down`；菜单条另有 `diagram-menu-close` 按钮 |
| 放大/缩小/复位 | `+`/`=`、`-`、`0` | `pane_key`（搜索框聚焦时不生效） |
| 悬停高亮关联 | `Enter`/`Shift+Enter` 会同时把该卡片设为当前卡片（`hovered`） | `card_key` |

## A2 修复：清除搜索要真的清掉输入控件

第一版 `clear_search` 除了 `self.search.clear()`，还顺手把 `field.sync` 一起清空。`search_field` 的回写条件是 `field.sync != wanted`——两边都变成空串后条件不成立，于是控件里留着旧文字：点“清除搜索”或框内按 Esc 后，提示条消失、画布恢复，但输入框仍是 “zzz”，下一次输入会接在旧词后面。

修复：`clear_search` 只改模型（`search`/`highlight`/`center_pending`），把回写交给既有的 `search_field` 路径（`reset_canvas` 一直依赖同一条路径）。

测试补强：三个宽度用例不再只看 `pane.search`，而是用 `search_control_text` 读 `InputState` 里的真实文字——条件出现时断言控件显示 “zzz”，点“清除搜索”后再断言控件是空串。只断言模型值正是这个 bug 溜过去的原因。

## 页头“返回”按钮的消费点

`DiagramPane::take_back()` 由 `src-tauri/src/app/shell/view.rs` 的 `consume_diagram_back`（约 2038 行）在渲染路径上取走，随后 `model.return_to_work()` 回到上一个工作项；这条链路已接通，不是缺口。

## 未覆盖项

- 原生像素截图未取得（见 `README.md` 的证据类型）。
- 搜索无命中是用模型状态直接摆现场（`search = "zzz"` + `highlight = None`）验证的，没有走“往输入框逐字键入”的驱动路径；搜索本身的行为（`focus_search`）未改动。
