# 管理页审计与返工（T15）

基准：`specs/2026-10-17-desktop-ui-ux-redesign-design.md`（设计语言未覆盖 admin，按最接近的既有规范类比），
底座：`src-tauri/src/ui/kit.rs`（T1 已落地）。审计前的问题来自探索阶段记录：
「操作卡与按钮使用 kit；inventory fault 自绘，probing 未用于渲染 loading，空 section 无下一步。」

## 1. 探测（probing）没有可见加载态

| 项 | 改动前 | 改动后 |
| --- | --- | --- |
| 状态字段 | `probing: bool` 只在异步回调里清掉，渲染从不读它 | 渲染按 `probing` 与 `inventory.probed` 两个事实分档 |
| `inventory.probed` 语义 | 发起请求时就置 true（"请求过"），读没读完看不出来 | 只在读取结束时置 true（读到的可能仍是离线结果），读取途中为 false |
| 首屏（还没读完） | 直接画空表格外的卡片，看不到任何读取指示 | 正文顶部 `kit::skeleton_state(3, &metrics, &p)` |
| 已读内容的刷新 | 无反馈（且旧代码 `refresh` 会先把 `probed` 清成未读，刷新时只会闪回首屏骨架） | `refresh` 不再清 `inventory.probed`，只清"读取中"再发起一次读取；正文顶部 `kit::inline_loading("正在重新读取清单…")`，已读到的清单与草稿都留着。首屏骨架只属于换目标/换工作项（`bind_item` 重建会话） |
| 表头 | 只有来源说明 | 读取中同一位置换成 `kit::inline_loading`（`admin-probing`），读完换回来源说明，表头不抖动 |

`request_inventory` 不再在发起时把 `inventory.probed` 置 true；渲染入口的重复发起判定改成
`!probing && !inventory.probed`，`note_connection` 的去抖条件同步补上 `probing`，避免同一连接状态重复探测。

## 2. inventory fault 自绘危险色卡片

| 项 | 改动前 | 改动后 |
| --- | --- | --- |
| 呈现 | `kit::card` + 自绘标题/消息/重试按钮，与其它页面三态不一致 | `kit::error_state_with_action`：危险色图标块 + 标题 + 原因 + 块内主操作 |
| 下一步 | 重试（保留） | 重试（`admin-retry-{index}`，文案 `admin.action.retry`，留在错误态的动作位） |
| 落点 | 每个失败分区一个块 | 不变，仍按 `admin-fault-{index}` 落在自己的分区，不混成一锅 |

## 3. 空 section 没有下一步

| 分区 | 改动前 | 改动后 |
| --- | --- | --- |
| 附加数据库 | 列表为空就什么都不画 | `admin-attach-empty`：`kit::empty_state` + "在下方填路径和名字，预览 SQL 并确认后才会 ATTACH" |
| 序列 | 同上 | `admin-sequence-empty` + "填序列名与起始值，预览 ALTER SEQUENCE 后再确认" |
| 事件 | 同上 | `admin-event-empty` + "填事件名并选择启用或停用，预览 SQL 确认后才生效" |
| 枚举 / 域 | 同上 | `admin-enum-empty` / `admin-domain-empty`，各带本节下一步 |
| 扩展与分区 | 一张只有来源说明和空 flex 的卡片 | 卡片加标题「扩展与分区」；读不到时 `admin-extras-empty` 提示先刷新或核对目标库/schema |
| 整页（引擎不支持） | `empty_state` + "这个引擎没有数据库级管理操作。"（一句结束语，没有下一步） | 同一空态，说明换成 `admin.section.noneHint`：切换到 SQLite / PostgreSQL / MySQL 目标后会列出对应操作 |
| 未连接 | 只有卡片 + 一行 note | `admin-offline`：`kit::empty_state_with_action` + 「立即连接」，说明"下方操作仍可预览 SQL，但确认也不会执行"；这个主操作真的会发起连接（见下） |

边界：空态只在**读过清单的已连接目标**上出现（`listed = inventory.probed && inventory.connected`）。
读取中不闪空态、离线不冒充"这一节是空的"，两类语义文案互不相同（见 `admin_state_copy_is_translated_and_state_specific`）。
空态放在本节卡片里，表单与「预览 SQL」入口仍在下方，管理能力没有因为空态消失。

### 空态主操作的接线（T15-A2）

管理页只表达意图：「立即连接」和表头的 `admin-connect` 都写 `AdminPane::connect_request` 并重绘。
外壳在同一帧的 `note_admin_link`（`src-tauri/src/app/shell/view.rs`，本来就负责同步管理页与连接状态）里
用 `take_connect_request()` 取走请求，交给**树和连接行已经在用的按需连接入口**
`model.queue_connect(ConnectIntent { action: Expand })` → `PendingJob::OpenConnection`。
连上后回到同一个 `note_admin_link` 的 `note_connection(true)`，管理页自己重读清单，用户仍停在管理页。
没有新增连接路径、没有重复排队：请求取走一次即清空。

证据：`app::shell::tests::admin_connect_action_enters_the_shared_on_demand_flow`——无头工作台里点
`admin-body-connect` 后，连接状态进入 `Connecting`、续接队列里恰好一条 `Expand`、`connect_request` 已清空，
再渲染一帧也不会重复排队。

## 4. 危险操作呈现

| 项 | 改动前 | 改动后 |
| --- | --- | --- |
| 确认层说明 | 只有风险行、规则行与阻断原因 | 再加一行固定说明：有 danger 级风险用 `admin.preview.danger`（危险色 `admin-preview-danger`），否则用 `admin.preview.irreversible`（提醒色 `admin-preview-irreversible`），都写明"确认后立即执行、数据库不提供撤销" |
| 确认按钮 | 恒为 primary | 计划带 danger 级风险时换 `kit::danger_button`；其余保持 primary。判定收在 `AdminSession::preview_dangerous`，与结构页同名同义 |
| 不变的边界 | 预览 → 确认 → 执行；阻断原因块；目标切换丢弃预览；失败保留预览 | 全部保留，未改执行路径（`confirm_steps` / `execute_request` 未动） |

## 5. 保持不回退的入口

- 菜单入口没有改动（`shell/menus.rs` 属链上任务，本任务未触碰）。
- inventory / PRAGMA 读取仍在 `read_inventory` / `inventory_from_extras` / `load_postgres_types`，本任务只改呈现。
- 目标切换：`bind_item` 的"同工作项切工具不丢草稿、换目标重建"逻辑未动，`admin_target_change_drops_the_previous_operation_and_preview` 通过。
- 预览失败保留：`failed_admin_confirm_keeps_the_preview_and_names_the_reason` 通过。
- 危险确认：确认按钮 id `admin-confirm` 与取消 `admin-cancel` 未变，`dangerous_previews_mark_the_confirm_and_explain_that_it_cannot_be_undone` 钉住。
