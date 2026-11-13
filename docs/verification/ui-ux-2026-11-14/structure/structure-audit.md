# 表结构页逐条审计（T9）

对照基准：`specs/2026-10-17-desktop-ui-ux-redesign-design.md` S6.1 / S8.4 / S8.5 / S12.2 / S12.5；
需求：`docs/requirements/2026-11-14-desktop-ui-ux-unification.md` R1/R3/R4/R5。

## 1. 字段 / 索引 / 外键三张列表（S8.4「复用结果网格的表头、行高、列宽与右键规范」）

| # | 本次之前 | 本次改动 | 验收对应 |
| --- | --- | --- | --- |
| 1 | 表头高 `bar_h - 2`、行高 `row_h + 2`、左右内边距各表写 `px(14.)`，三张表各自拼装 | 新增 `TableSpec`：表头高与行高都取 `Metrics::row_h`（紧凑 26 / 标准 32 / 宽松 40），内边距统一 `12`；三张表共用 `structure_table` 一个构建函数 | 列表行高、列宽一致（T9 outcome 1）；S12.2 |
| 2 | 列宽是散落的 flex 权重（`vec![2., 1.4, 0.6, 1.4]` 之类），没有最小宽度 | 每列声明 `ColumnSpec { key, weight, min_w }`，作为具名常量 `FIELDS_TABLE / INDEXES_TABLE / FOREIGN_TABLE`；最小宽度之和在 640 宽窗口内放得下 | 三档宽度可用、不横向溢出（R6）；outcome 1 |
| 3 | 行只有 hover，没有右键菜单（S6.1 要求行级右键） | 每行挂 `context_menu`：字段（复制名称 / 标记删除此字段或取消标记）、索引（复制名称 / 删除该索引，受保护索引禁用并带原因）、外键（复制名称 / 删除外键） | 行右键可用（outcome 1）；S6.1 |
| 4 | 空列表 = 表头 + 一行 `—`（"无意义表头加破折号"，探索阶段已记录） | 空态整块替换内容区：`kit::empty_state` 按页签给图标/标题/下一步说明，**不画表头**；`shows_table_header` 只对 `ListState::Rows` 返回真 | outcome 1；S8.5 |
| 5 | 行内 "可空/唯一" 用 `✓`/`—`，但列名/类型没有截断约束 | 单元格统一 `truncate`，列最小宽度保证长名不把表撑破 | R6 不横向滚动 |

菜单动作的语义边界（R5）：复制只写剪贴板；"标记删除此字段"只改 `kept_columns` 的 `dropped` 标记（列表行同时淡化），
"删除该索引/外键"只设置 `EditIntent` 并发起预览。三者都不执行任何 SQL——执行仍要经过预览 → 确认。

## 2. 三态：读取加载 / 读取失败 / 空对象（S8.5、S12.4）

| # | 本次之前 | 本次改动 | 对应 |
| --- | --- | --- | --- |
| 1 | 读取中：`loading_state` 整块转圈，且只对列表页签生效 | 首屏读取改用 `kit::skeleton_state(6, …)`：按当前密度排 6 行骨架，读到数据后换真实行；不再用整块转圈盖住卡片 | S12.4 / R4 |
| 2 | 读取失败：`div().p(16).text_color(danger)` 一行裸文字，没有下一步 | `kit::error_state_with_action` + `重试` 按钮（`structure-retry`）：重试清掉 `load_error`/`loaded_for` 后重新读取，**不清用户草稿** | S8.5「对象读取失败（带重试）」 |
| 3 | 没有快照时：`ui.structure.none` 一行弱文字 | 按页签给 `kit::empty_state`：字段/索引/外键各自的"还没有…"与下一步说明；定义类页签给"这个对象没有可显示的 SQL 定义" | S8.5「无行与无命中文案不同」 |
| 4 | 读取中/失败时仍然渲染新增表单与预览按钮，用户可以先点一个注定失败的操作 | `form_visible()`：加载中或失败时不渲染新增表单，也不渲染主操作栏；先看状态与重试 | R3 四态一致 |
| 5 | 三态判定一度只落在列表页签里：视图 / DDL / 危险操作页签上读取失败会退化成"这个对象没有可显示的 SQL 定义"（无原因、无重试），读取中还会先闪这句错话 | `overview()` 把 加载 → 失败 → 空 → 内容 的判定提到页签分支之前，由同一个 `list_state` 决定；列表页签的"空"看行数，定义类页签的"空"看 `has_definition()`（DDL 页签用重建定义，所以与 `table_sql` 是否为空不完全一致）。失败态统一由 `load_error_state()` 给原因 + 重试 | R3 / S8.5；回归 `definition_tabs_share_the_loading_and_load_failure_states` |

判定集中在 `list_state(loading, error, rows)`：加载优先于错误，错误优先于空态；`empty_lists_have_no_header_and_loading_wins_over_error` 反向钉住，`definition_tabs_share_the_loading_and_load_failure_states` 钉住"所有页签同一套三态"。

### 文案键必须真的存在

`i18n::text` 对未知键原样返回键名，而字典自测只从"键→语言"方向检查，抓不到"用了不存在的键"。
本页曾经把 `structure.error.retry`（字典里没有）直接显示成"structure.error.retry"，因此：
读取失败的重试按钮改用既有的 `common.retry`，并新增 `every_structure_copy_key_used_in_the_source_exists_in_the_catalog`
反向扫描本文件里所有 `structure.* / ui.structure.* / common.* / admin.* / catalog.*` 形式的字符串字面量，逐个要求出现在 `i18n::catalog_keys()` 里。
该用例在把坏键临时写回源码时会失败（已实测：报 `"structure.error.retry" 被当作文案键使用，但字典里没有它`），恢复后通过。

## 3. 主操作可达（R6 三档宽度）

| # | 本次之前 | 本次改动 |
| --- | --- | --- |
| 1 | 「预览 SQL」按钮在新增表单卡片末尾，随字段列表一起滚；字段多、窗口 640 宽时要在滚动区里找 | 移到页签内容之外的固定动作栏 `structure-actions`（`flex_none`、上发丝线、surface 底），左侧一句"预览只生成 SQL，确认后才执行"，右侧主按钮；危险页签用危险样式 |
| 2 | 表头只有「表名 + 连接/库/环境」一行 banner | 增加数量摘要行 `structure-summary`（"N 个字段 · M 个索引"），S8.4 要求 |

headless 证据：`primary_action_stays_outside_the_scrolled_body_at_every_width` 在 1360/900/640 × 900 下断言
主按钮底边不越出窗口、且顶边不早于 `structure-body` 的底边（即不在滚动内容里），并断言行高等于 `Metrics::row_h`。

## 4. 危险操作确认层（S12.5，R5 数据安全边界）

| # | 本次之前 | 本次改动 |
| --- | --- | --- |
| 1 | 单一 `flex_col` 面板：标题、SQL、风险、阻断、生产环境输入框、按钮全在一条竖向流里；640×640 时按钮被挤出可视区 | 三区结构：固定标题行（危险图标 + 标题 + 对象名）、可滚动正文（`structure-preview-body`：banner、SQL 块、目标、风险、规则、阻断、提示、生产环境输入框）、固定页脚（取消 + 确认）；面板 `max_h(relative(0.92))`、`overflow_hidden` |
| 2 | 危险样式只按"当前是危险页签"判断 | `preview_dangerous()`：危险页签**或**计划里存在 `danger` 级风险；标题图标与确认按钮配色跟随它 |
| 3 | 生产环境禁用条件内联在渲染里 | `confirm_blocked()`：危险页签且对象名不匹配时确认按钮禁用；未匹配时正文给"输入的对象名完全一致后才能确认执行"的提示 `structure.danger.typeHint` |
| 4 | 对象名只出现在 banner | 标题行右侧显示 `object_label()`（已有对象用表名，新建表用输入的表名） |

**没有改动的语义**（outcome 3）：`requires_second_confirm`（第一次确认只 arm、第二次才执行）、`cancel()` 重置 armed、
`confirm_steps()` 的阻断/未连接/未预览拒绝、`danger_name_ok()` 的完全匹配比较、`application::ddl` 的预览/执行入口与"未确认不执行"都在原处，
原有回归用例全部保留并通过。

## 5. 未解决事实

1. **原生像素截图未取得**：环境内没有原生窗口/截屏通道；`浅/深色 × 紧凑/标准 × 1360/900/640` 的 PNG 缺，
   需 Delivery 在最终集成 candidate 上补。本目录的布局证据全部来自 headless GPUI 测试窗口，不是原生像素证据。
2. **ContextMenu 弹层没有渲染级用例**：库的 `ContextMenu` 在 headless 测试窗口里不展开（它用 `hitbox.is_hovered` 判断按下是否属于自己，没有真实指针时恒为假），所以右键菜单验证的是"菜单项内容 + 动作语义"（`row_menu_actions_only_edit_the_draft_and_never_execute`），行本身的渲染与行高由布局级用例覆盖；原生窗口下的右键展开仍需人工/Delivery 观察。
3. **索引/外键行的右键菜单没有渲染级用例**：`ddl::model` 的 `Index`/`IndexColumn`/`ForeignKey` 类型是私有模块（`ddl/mod.rs` 的 `mod model;`），
   结构页无法在测试里构造带索引/外键的快照；生产路径 `Index` 数据只能由 `ddl::introspect::load` 产生。
   因此渲染级用例覆盖字段行菜单（同一份 `structure_table`/`menu_entry` 代码路径），索引/外键菜单只有列规格与空态判定被钉住。
   若需要同等渲染证据，需要在 `ddl` 侧导出这些类型或提供测试构造器——属于本任务范围外的接口变更，未擅自改动。
4. **MySQL/PostgreSQL/MariaDB 容器未启动**：本任务没有涉及连接行为，文件级 SQLite 用例已覆盖预览/执行/取消。
