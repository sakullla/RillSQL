# 管理页三态与三档宽度（T15）

证据类型：**源码级 + headless 布局级**，不是原生像素截图（见 `README.md`）。

## 1. 三态判定

管理页的"内容"是**异步读到的清单**（PRAGMA、附加库、序列、事件、枚举、域、扩展、分区），
表单与预览入口来自本地草稿，不等外部读取，因此三态只替换"清单这一块"，不吞掉操作卡。

| probing | inventory.probed | connected | 状态 | 呈现 | 页面其余部分 |
| --- | --- | --- | --- | --- | --- |
| 是 | 否 | 未读完，按离线 | Loading（首屏） | `kit::skeleton_state(3, …)` | 表头 `admin-probing` 转圈 + 全部操作卡 |
| 是 | 是 | 任意 | Loading（增量） | `kit::inline_loading`（`admin-refreshing`） | 已读到的清单、草稿、卡片都留着 |
| 否 | 是 | 否 | NoData（未连接） | `kit::empty_state_with_action`（`admin-offline`）+ 「立即连接」 | 操作卡仍在，说明"预览不会执行" |
| 否 | 是 | 是 | Error（某分区读取失败） | `kit::error_state_with_action`（`admin-fault-{i}`）+ 「重试」 | 其余分区照常 |
| 否 | 是 | 是 | NoData（某分区为空） | 该节卡片内 `kit::empty_state`（`{id}-empty`） | 该节的表单与「预览 SQL」仍在其下 |

优先级：加载 > 错误 > 空。理由分别是：迟到的错误不该盖住正在读的状态；读取失败不能退化成
"这里没有序列"；读取中不能先闪空态。

各分区空态 id 与文案键：

| 分区 | 块 id | 标题键 | 说明键 |
| --- | --- | --- | --- |
| 附加数据库 | `admin-attach-empty` | `admin.empty.attached` | `admin.empty.attachedHint` |
| 序列 | `admin-sequence-empty` | `admin.empty.sequences` | `admin.empty.sequencesHint` |
| 事件 | `admin-event-empty` | `admin.empty.events` | `admin.empty.eventsHint` |
| 枚举 | `admin-enum-empty` | `admin.empty.enums` | `admin.empty.enumsHint` |
| 域 | `admin-domain-empty` | `admin.empty.domains` | `admin.empty.domainsHint` |
| 扩展与分区 | `admin-extras-empty` | `admin.empty.extras` | `admin.empty.extrasHint` |
| 整页（引擎不支持） | 底座 `empty-state` | `ui.tool.admin` | `admin.section.noneHint` |

## 2. 危险操作呈现

| 事实 | 呈现 |
| --- | --- |
| `preview_dangerous()` 为真（计划里有 `danger` 级风险） | 提示行 `admin-preview-danger`（危险色 `danger_soft`/`danger`，文案 `admin.preview.danger`）+ 确认按钮 `kit::danger_button` |
| 为假（普通管理操作） | 提示行 `admin-preview-irreversible`（提醒色 `warn_soft`/`warn`，文案 `admin.preview.irreversible`）+ 确认按钮 primary |

两条文案互不相同，`admin_state_copy_is_translated_and_state_specific` 断言它们在中文下不同且英文不含中文
（`preview_dangerous` 的判定在 `dangerous_previews_mark_the_confirm_and_explain_that_it_cannot_be_undone` 里钉住）。

## 3. 三档宽度 × 双主题 × 双密度（headless 实测几何）

窗口 1360 / 900 / 640 × 900，页面为 sqlite 目标、清单已读完但附加库为空（空态在场）。
下表是 `states_and_actions_stay_inside_1360_900_and_640_in_both_themes_and_densities` 打印的实际边界
（内容列 `max_w(760)` 居中，所以 1360 下卡片不铺满整宽）：

| 主题/密度 | 宽 | 空态说明 (x, w) | 预览入口右边界 | 刷新按钮右边界 |
| --- | --- | --- | --- | --- |
| 浅色/紧凑 | 1360 | 327, 360 | 1045 | 1344 |
| 浅色/紧凑 | 900 | 97, 360 | 815 | 884 |
| 浅色/紧凑 | 640 | 43, 360 | 609 | 624 |
| 浅色/标准 | 1360 | 327, 360 | 1045 | 1340 |
| 浅色/标准 | 900 | 97, 360 | 815 | 880 |
| 浅色/标准 | 640 | 47, 360 | 605 | 620 |
| 深色/紧凑 | 1360 | 327, 360 | 1045 | 1344 |
| 深色/紧凑 | 900 | 97, 360 | 815 | 884 |
| 深色/紧凑 | 640 | 43, 360 | 609 | 624 |
| 深色/标准 | 1360 | 327, 360 | 1045 | 1340 |
| 深色/标准 | 900 | 97, 360 | 815 | 880 |
| 深色/标准 | 640 | 47, 360 | 605 | 620 |

结论：

- 12 种组合下，空态说明、预览入口、刷新按钮的右边界都 ≤ 窗口宽（640 最紧的是刷新按钮 624/620），没有横向滚动。
- 主题只换配色，不换几何（浅/深色同密度数值一致）；密度只换内边距与行高（空态说明的 y 由 600 → 614，刷新按钮 x 与 y 各差 4），
  与 `kit::Metrics` 的 `page_pad` / `row_h` 一致。
- 窄窗口（640）不做左右分栏重排：管理页本来是一列卡片（`max_w(760)`、`mx_auto`），640 下只是把卡片收窄，
  没有需要切换的并排结构。

## 4. 未覆盖的呈现

- `pills` 行（已安装扩展、分区、附加库名）在 640 宽下依赖 `flex_wrap`，本任务的宽度用例选的是空的附加库分区，
  没有覆盖"名字很长且很多"的极端组合；这类内容由 `kit::pill` + `flex_wrap` 兜底，未在本任务单独压测。
- 预览对话框固定 `w(600)` + `max_w(relative(0.92))`：640 下按 92% 收窄，本任务没有改它的宽度策略。
