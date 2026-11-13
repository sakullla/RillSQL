# Redis 四态与三档宽度（T14）

证据类型：**源码级 + headless 布局级**，不是原生像素截图（见 `README.md`）。

## 1. 四态判定表（`list_state`）

判定输入六个：`busy`（是否在读取）、`scan_failed`（错误来源是 SCAN）、`total`（SCAN 已读到的键总数）、
`visible`（类型/命名空间过滤后实际渲染的行数，树形视图里含目录行）、`pattern_active`（模式框是否带条件，
SCAN 在服务端按它过滤，所以命中 0 时 `total` 也是 0）、`done`（这一轮 SCAN 的游标是否已读完）。

| busy | scan_failed | total | visible | pattern_active | done | 状态 | 呈现 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 是 | 任意 | 0 | 0 | 任意 | 任意 | Loading | `kit::skeleton_state(6, …)` |
| 否 | 是 | 0 | 0 | 任意 | 任意 | Error | `kit::error_state_with_action` + 「重试」 |
| 否 | 否 | 0 | 0 | 否 | 否 | Rows（空行） | 不画状态块，只给一行弱提示（`redis.tab.emptyList`），页脚保留「再读取一批（SCAN）」 |
| 否 | 否 | 0 | 0 | 是 | 否 | Rows（空行） | **同上**：游标还开着就不下「无命中」结论，模式与页脚都保留 |
| 否 | 否 | 0 | 0 | 是 | 是 | NoMatch | `kit::no_match_state_with_action` + 「清除筛选」 |
| 否 | 否 | 0 | 0 | 否 | 是 | Empty | `kit::empty_state_with_action` + 「新建键」 |
| 否 | 否 | >0 | 0 | 任意 | 任意 | NoMatch | `kit::no_match_state_with_action` + 「清除筛选」 |
| 任意 | 任意 | >0 | >0 | 任意 | 任意 | Rows | 行 + （读取中时）`kit::inline_loading` |

优先级：加载 > 错误 > **是否读完** > 无命中 > 无数据。理由分别是：迟到的错误不该盖住正在读的状态；
读取失败不能退化成「这里没有键」；SCAN 用服务端 `MATCH` 分批返回（`redis::keys::scan_keys`），
**空页是正常现象**——游标还开着就说「这一轮已读完 / 没有符合条件的键」，等于把未完成的扫描当结论，
既会误导用户，也会让用户的模式连同页脚的「再读取一批」一起被废掉，所以 `total == 0` 里先看 `!done`；
读完之后的 0 命中才是「没命中」（下一步清除条件），读完后无模式条件的 0 键才是「库里没有键」，
后者若错说成前者，代价是把空态主操作错配成「新建键」并藏起「清除筛选」。
`key_list_state_keeps_busy_error_no_match_and_empty_apart` 钉住整张表，
`empty_state_needs_a_finished_scan_and_no_pattern` 钉住三条渲染级结论：读完+带模式 → 无命中态；读完+无模式 → 空态；
没读完（有无模式都一样）→ 不画任何状态块、页脚保留「再读取一批」、列表里只有一行弱提示。

已读到行、但下一页 SCAN 失败时（`total > 0` 且 `scan_failed`）：行保留，键列表底部换成错误条 + 「重试」（`redis-retry-scan`），
选中键与命令草稿都不动——失败不伪装成「键没了」。

## 2. 错误落点（`ErrorSource`）

| 来源 | 触发点 | 呈现位置 | 下一步 |
| --- | --- | --- | --- |
| `Scan` | `load_keys` 无连接、`finish_scan` 失败、重试扫 | 键列表 | 重试（`redis-retry-scan`） |
| `Detail` | `select_key` 无连接、`finish_read` 失败 | 值区域 | 重试读取（`redis-retry-read`） |
| `Command` | 命令门禁、`finish_exec` / `finish_mutate` / `finish_delete` / `finish_ttl` 失败、草稿校验 | 命令行 | 关闭提示（`redis-error-dismiss`），命令原文留在输入框 |

底部状态行（`redis-status`）继续用 `status_line()` 作为无障碍名，可见文字只放结果说明，
同一段错误不在错误块与状态行重复堆叠。

## 3. 窄窗口重排

断点沿用底座：`kit::NARROW_PAGE_WIDTH = 760.`（不自造常量）。

| 项目 | 宽窗口（≥ 760） | 窄窗口（< 760） |
| --- | --- | --- |
| 结构 | `row(键列表, column(值区域, 命令行))` | `column(切换条, 键列表 *或* 值区域, 命令行)` |
| 键列表宽度 | `header_h * 6 + pane_pad`：紧凑 262 / 标准 300 / 宽松 340 | 占满整列（`w_full`） |
| 值区域 | 剩余宽度 | 占满整列 |
| 切换 | 无 | `redis-nav-strip` + `redis-nav-toggle`（键列表 / 键内容） |
| 命令行 | 右栏底部 | 面板底部（与内容区同级，切换时不消失） |
| 选中键后 | 右侧显示内容 | 自动转到值区域（`select_key` 按 `viewport_w` 判定） |

640 宽时按旧布局值区域只有约 378（标准密度 300 侧栏 + 间距），TTL 与重命名两列表单各分到约 180；
重排后两栏都拿到 640 的整宽，表单与值内容不再被挤成窄缝。

`keys_visible` 的三条规则（钉在 `narrow_window_swaps_the_key_list_and_the_value_area` 与 `state_actions_drive_the_model`）：

1. 宽窗口无条件显示键列表——不保留窄窗口里手动收起的残留选择；
2. 窄窗口未选中键时先显示键列表（否则进来就是「还没选键」的死角）；
3. 窄窗口用户显式切换过就听用户的（`nav_choice`）。

## 4. 密度与双主题

| 项目 | 紧凑 | 标准 | 宽松 | 取值 |
| --- | --- | --- | --- | --- |
| 键行高 | 25 | 32 | 38 | `Metrics::tree_row_h + 2` |
| 目录行高 | 23 | 30 | 36 | `Metrics::tree_row_h` |
| 列表内边距 | 4 | 8 | 12 | `Metrics::pad` |
| 侧栏宽 | 262 | 300 | 340 | `Metrics::header_h * 6 + pane_pad` |
| 状态块 | 图标块 30 + 左对齐文字 + 主操作左缩进 38 | 同左 | 同左 | `kit::STATE_ACTION_INSET` |

Redis 页没有自己的颜色常量：键行、类型 pill、选中底、danger 块全部来自 `Palette` 语义色
（`chrome / line / hover / selection / danger_soft / accent_soft`），主题只换配色，几何只随密度变。

headless 证据：`redis_pane_holds_both_themes_both_densities_and_three_widths` 在**浅色/深色 × 紧凑/标准 × 1360/900/640**
12 个组合下各开一个窗口，断言面板宽不超过窗口宽（无横向溢出）、宽窗口键行高等于 `tree_row_h + 2`、
切换条只在窄窗口出现、窄窗口值区域在位。
`token_tests::style_literals_follow_design_tokens`（全量测试中）继续保证本文件没有裸字号/裸圆角。

该用例是 **headless 布局级**证据，不能证明两套主题的实际渲染色块；像素证据由 Delivery 采集。
