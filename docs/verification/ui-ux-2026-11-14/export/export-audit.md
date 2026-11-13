# 导入导出抽屉审计与修复（T12）

对象：`src-tauri/src/app/views/export/mod.rs`。基线：T1 的 `src-tauri/src/ui/kit.rs`（`empty_state(_with_action)`、`inline_loading`、`interactive_states`、`Metrics`、`Palette`）。本任务只改呈现、状态判定、禁用语义与响应式动作，不改导入导出执行、文件写入、取消与任务记录语义。

01 探索记录里对本页的记录是：「抽屉宽度钳制、occlude、Esc/外点关闭已有；running 裸 Spinner，无表/无结果/任务空态自绘，部分 disabled 仅 opacity。」下表按这条逐项对账。

| # | 源码入口（修复前） | 偏差 | 修复结果 | 验证材料 |
| --- | --- | --- | --- | --- |
| 1 | 查询来源的分支里 `Spinner::new().xsmall()` + 一行文字（旧 `mod.rs` 1140–1160）；表来源另有一份同样的裸 Spinner（旧 1173–1182） | 同一个"来源还在读"的状态有两份各写一遍的实现，用的不是底座入口；只有状态没有下一步 | 合并为一条 `kit::inline_loading(t(self.running_key()), &metrics, &p)`，并在下一行给出 `ui.exchange.next.waitRead`（`mod.rs:1260`–`1264`、`running_key` 在 `1003`） | `source_state_separates_running_no_table_no_result_and_ready`、`exchange_states_hold_at_*`（`inline-loading` 存在、`empty-state` 不存在） |
| 2 | 表来源没有当前表时自绘一行 `text_faint` 灰字（旧 1120–1128） | 空态自己画：没有图标块、没有标题、没有下一步，和页面其它空态不是一套排版 | 改 `kit::empty_state`（`IconName::Table` + `ui.exchange.state.noTableTitle` + `ui.exchange.noTableData`）。这一态**不给按钮**：抽屉里的"重新读取来源"产不出表来，下一步（在对象树里打开一张表）写在说明里；"重新读取来源"只留给真能恢复的 NoResult（见 #3） | `exchange_states_hold_at_*`（`empty-state` + `empty-state-detail` 存在，`state-action` 与 `exchange-source-refresh-no-table` **不存在**） |
| 3 | 查询来源没有结果时自绘一行灰字（旧 1146–1153） | 与 #2 同一句排版，但原因和下一步完全不同，代码里只有一句灰字 | 同样是 `kit::empty_state_with_action`，标题 `ui.exchange.state.noResultTitle`，说明按来源取 `ui.exchange.noResult` / `ui.exchange.noTableColumns`（`no_result_key` 在 `1011`），动作位是"重新读取来源"——只在这一态出现，因为重新读取确实可能补上结果 | `exchange_states_hold_at_*`（`exchange-source-refresh-no-result`）；`state_copy_differs_per_case_and_english_has_no_chinese`（两句文案不相同） |
| 4 | `blocked_row` 只有 `.opacity(0.55)`，原因是一行小字（旧 1424–1448） | 禁用只有"变淡"：没有 not-allowed 光标、没有"不可聚焦"的语义、悬停仍无反馈但也没有说明；原因和下一步挤在一行，读屏只听到标题 | 改 `kit::interactive_states(div().id(id), p, true)`（不透明度 0.55 + `cursor_not_allowed` + 不设 `tab_index`，即不可聚焦），标题下分成"原因"与"下一步："两行，`aria_label` 为"标题：原因 下一步…"（`mod.rs:1619`–`1650`） | `exchange_states_hold_at_*` 里对 `label()` 的断言（原因含"读取/没有结果/没有当前表"，且含"下一步"） |
| 5 | 全量导出被挡住时统一给 `ui.exchange.fullUnsupported`（旧 1258–1268） | 三种原因（查询来源、来源在执行、没有当前表）说成一句泛化文案，用户不知道自己属于哪一种 | 新增 `full_block_key`：`fullQueryOnly` / `fullSourceRunning` / `fullNoTable`（`mod.rs:1019`–`1027`） | `blocked_actions_name_the_reason_and_the_next_step`（查询 → `fullQueryOnly`，无表 → `fullNoTable`） |
| 6 | 查询来源时全库导出/迁移只给一句 `dumpUnsupported`/`migrateUnsupported`；表来源没有当前表时它们仍是可点的 `action_row`，点了才 `block()`（旧 1270–1291） | 两种问题：原因是泛化的一句话；"没有表"这条路径先给可点入口再在别处失败，等于让用户白点一次 | `blocked_reason` 按来源给出 `dumpQueryOnly`/`dumpNoConnection`、`migrateQueryOnly`/`migrateSourceRunning`/`migrateNoTable`（`mod.rs:1030`–`1052`）；无表或查询来源时这两条直接渲染成 `blocked_row`，只有表就绪才渲染 `action_row`；表在执行时全库导出可点、迁移挡住（`mod.rs:1391`–`1440`），后者与 #11 收窄后的执行守卫一致 | `blocked_actions_name_the_reason_and_the_next_step`（三种来源逐一断言原因键）；`exchange_states_hold_at_*`（无表时 `exchange-exchange-dump` 的 label 含"没有当前连接"与"下一步"） |
| 7 | 任务空态是一行灰字 `export.task.empty`（旧 1400–1403） | 空态没有下一步：用户看不出"这里将出现什么"和"怎么让它出现" | 改 `kit::empty_state(_with_action)`：标题 `ui.exchange.state.tasksTitle`、说明 `ui.exchange.state.tasksDetail`；来源就绪时空态带"导出已读取的行"动作，未就绪时只说明不摆动作（`mod.rs:1540`–`1552`） | `exchange_states_hold_at_*`（`exchange-tasks` 内 `empty-state` + `exchange-tasks-export`）、`tasks_empty_state_drops_the_export_action_when_the_source_is_not_ready` |
| 8 | "取消任务""清除已结束的"始终可点（旧 1455–1462） | 没有可取消/可清除的任务时按钮照旧可点，点了什么也不会发生——正是"部分 disabled 仅 opacity"的反面：连禁用态都没有 | `kit::tool_button(..).disabled(!任意运行中)` / `.disabled(!任意已结束)`（`mod.rs:1591`–`1610`） | `exchange_states_hold_at_*`（`disabled() == Some(true)`） |
| 9 | 查询来源的导入区是自绘标题 + 说明，没有动作（旧 1326–1337） | 说明写清了"为什么不能写回"，但没有出路：用户得自己去上面找分段控件 | 改 `kit::empty_state_with_action`，主操作"切到当前表数据"直接走 `request_source("exchange-source:table")`（`mod.rs:1443`–`1476`） | `exchange_states_hold_at_*`（`exchange-import-card` 内 `empty-state` + `exchange-import-use-table`） |
| 10 | 面板 `p(px(metrics.page_pad))` 用未钳制的密度值；导入与任务的动作行 `justify_end` 不换行；三个卡片容器没有 `min_w(0)`（旧 1189–1200、1330、1455） | 窄窗口里边距不收敛、动作行可能把内容挤出面板、卡片里的长文本可以撑开宽度 | `Metrics::of(cx).for_viewport(视口宽)`（`mod.rs:1174`），两处动作行加 `.flex_wrap()`（`1525`、`1597`），来源/导出/导入/任务四个容器加 `min_w(px(0.)) + max_w_full()`（`1340`、`1364`、`1443`、`1540`） | `exchange_states_hold_at_*` 的 `assert_inside_drawer`（抽屉宽 = min(520, 视口-60)，动作元素不越过左右边缘） |
| 11 | `prompt_save` 对**所有** SaveKind 在 `self.source.running` 时直接 `block()`（旧 508–527），而面板在表来源执行中仍把「全库导出」画成可点动作 | 面板宣传了一个永远打不开保存对话框的入口——正是本任务要消灭的"点了没反应"；同一分支里紧邻的「跨库迁移」却是被挡住的行，两条动作对同一个守卫给出相反说法 | 把运行中守卫收窄到「导出已读取的行」与「导出全部结果」：这两条写的是"当前读到的行"，读取中必须等。`start_dump` 只用 `source.table` 的 connection/engine/database 去导整个库（`mod.rs:746`–`766`），与已读取的行无关，所以读取中确实可以开始（`prompt_save` 的守卫在 `513`–`521`）。呈现与守卫现在一致：读取中全库导出是可点动作、跨库迁移是被挡住的行 | `dump_action_matches_its_execution_guard_while_the_source_is_running`：读取中 `exchange-exchange-dump` 的 label 只有标题（可点动作），走 `prompt_save(Dump)` 真的弹保存对话框且不产生 Blocked 记录；同一时刻 `prompt_save(Loaded)` 被挡下且不弹对话框 |

## 状态判定（唯一的四种呈现）

| 状态 | 判定 | 呈现 | 下一步 |
| --- | --- | --- | --- |
| Running | `source.running` | 标题行 + `kit::inline_loading`（局部加载，不整块换掉面板） | `ui.exchange.next.waitRead`：等这次读取结束后重试 |
| NoTable | 来源是表且没有当前表 | `kit::empty_state`（`IconName::Table`，无动作位） | 说明里写明"在对象树里打开一张表"；抽屉里没有再读取的动作，因为重读产不出表 |
| NoResult | 有目标但没有可用列 | `kit::empty_state_with_action`（`IconName::ListOrdered`） | `exchange-source-refresh-no-result`；说明区分"查询还没执行"与"表的列信息还没读到" |
| Ready | 有列且不在运行 | 已读取行数（+ 查询来源的"只有已读取的行"） | 导出/全库导出/迁移按各自能力给出行动作或被挡住的行 |

四种状态互斥，判定只有 `source_state()`（`mod.rs:989`–`1001`）一处；运行中优先于空态（加载优先于空，S8.5）。

## 被挡住的行动作（原因 + 下一步）

| 行动作 | 原因来源 | 下一步来源 |
| --- | --- | --- |
| 导出已读取的行 | `blocked_reason(ExportLoaded)`：运行中 → `running_key`；查询无结果 → `queryEmpty`；表无数据 → `noTableData` | `blocked_next`：运行中 → 等读取结束；无表 → 打开一张表；无结果 → 执行查询；表的列信息缺 → 重新读取来源 |
| 全量导出 | `full_block_key`：查询来源 / 运行中 / 没有当前表 | 同上；查询来源就绪时 → 切到当前表数据 |
| 全库导出 | `dumpQueryOnly` / `dumpNoConnection` | 同上 |
| 跨库迁移 | `migrateQueryOnly` / `migrateSourceRunning` / `migrateNoTable` | 同上 |

禁用语义统一由 `kit::interactive_states(.., true)` 提供：不透明度 0.55、`not-allowed` 光标、不进入 Tab 序列、无 hover/active；可访问名称是"标题：原因 下一步…"，读屏听到的和屏幕上看到的是同一句话。

## 没有改动的东西（边界）

| 行为 | 位置 | 本轮的处理 |
| --- | --- | --- |
| 抽屉遮挡下层控件 | `chrome.rs:4270`–`4274`（`.occlude()`） | 未改；面板整体的 `exchange` 元素仍是唯一 Dialog 角色容器 |
| 宽度钳制 | `chrome.rs:4277`（`520.0_f32.min(视口-60)`） | 未改；headless 用例按同一公式搭抽屉框并断言 |
| Esc 关闭 | `chrome.rs:300`–`304` | 未改 |
| 外点关闭 | `chrome.rs:4302`–`4310`（backdrop `on_mouse_down`） | 未改；面板内部 `stop_propagation` 仍在 |
| 内部滚动 | `chrome.rs:4299`（`exchange-scroll` + `overflow_y_scroll`） | 未改 |
| 后台写文件 / 取消 / 文件结果 | `launch_loaded`、`cancel_current`、`start_loaded`（`mod.rs:657`–`1000` 区域） | 未改；既有 `background_export_tests` 与 `mod tests` 用例继续覆盖 |
| `prompt_save` 的运行中守卫 | `mod.rs:513`–`521` | **改了**（唯一一处执行路径变化）：守卫从"所有 SaveKind"收窄到 Loaded/Full，理由与验证见 #11；`start_dump`、`spawn_dump`、`launch_loaded`、`cancel_current` 本身未改 |

## 未覆盖项

- 原生像素截图未取得（见 `README.md` 的证据类型）。
- 触发"重新读取来源"与"切到当前表数据"这两个新按钮时，面板只把请求交回外壳（`request_source` → `Workbench::perform`）；本轮没有在 headless 用例里接外壳，因此这两条路径的端到端结果由交付阶段在完整应用里复核（面板侧只断言按钮存在、可聚焦、id 唯一）。
- 导入预览态（`preview` 非空）的三按钮页脚只做了换行与 `min_w(0)`，没有造一份真实 `ImportPreview` 现场来断言其几何；其行为（取消/重选/确认）未改动。
