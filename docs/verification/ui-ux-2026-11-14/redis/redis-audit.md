# Redis 键浏览逐条审计（T14）

对照基准：`specs/2026-10-17-desktop-ui-ux-redesign-design.md` S5 / S8.5 / S12.2 / S12.4 / S12.5；
需求：`docs/requirements/2026-11-14-desktop-ui-ux-unification.md` R1/R3/R4。

## 1. 键列表四态（S8.5 / S12.4，R4）

| # | 本次之前 | 本次改动 | 验收对应 |
| --- | --- | --- | --- |
| 1 | 键列表为空时只有一行弱色文字「没有读到键。」，与「筛选没命中」「读取失败」共用同一句话 | 引入 `list_state(busy, scan_failed, total, visible, pattern_active, done)`，把 加载 / 读取失败 / 无命中 / 无数据 / 有行 分开判定；模式条件与「这一轮读完没有」都参与判定——带模式扫出 0 个键是「没命中」（下一步清除条件），游标没读完则保留页脚「再读取一批」而不宣布空库 | outcome 1；R4 三类空态语义 |
| 2 | 首屏读取没有本屏 loading 分支，`busy` 只影响命令门禁 | 键数为 0 且正在读 → `kit::skeleton_state(6, …)`；已读到行再读下一页 → `kit::inline_loading`，不清空已读到的键 | outcome 1；R4「加载优先用骨架/增量」 |
| 3 | 无数据空态没有下一步 | `kit::empty_state_with_action` + 「新建键」（`redis-new-key`，主按钮） | outcome 1；R4「空态以主操作作下一步」 |
| 4 | 无命中也走同一句空话 | `kit::no_match_state_with_action`：说明「这一轮 SCAN 已读完，但模式与类型过滤都没命中」，主操作「清除筛选」（`redis-clear-filter` 清模式+类型过滤后从游标 0 重扫） | outcome 1；R4 三类语义互不相同 |
| 5 | SCAN 失败只落在底部一行红字 | 键数为 0 时 `kit::error_state_with_action` + 「重试」（`redis-retry-scan`）；已读到行时底部换成错误条 + 重试，不清空列表、不动选中键 | outcome 1；S8.5「读取失败带重试」 |
| 6 | 判定不看「这一轮读完没有」：`done == false` 且这一页 0 个键时，列表区只剩页脚，空得没有一句解释 | `list_state` 收 `pattern_active` / `done`：`total == 0 && !done` 不下任何结论，只画一行弱提示（`redis-keys-blank`，文案复用 `redis.tab.emptyList`），下一步仍是页脚的「再读取一批（SCAN）」；读完才允许出现无命中或空态 | outcome 1；R4 |

## 2. console 错误与未选键（outcome 1 的后两条）

| # | 本次之前 | 本次改动 |
| --- | --- | --- |
| 1 | 所有失败共用 `error` 一个字符串，只在底部状态行显示成红字，没有来源区分 | 新增 `ErrorSource { Scan, Detail, Command }`：SCAN 失败进键列表，读键内容失败进值区域，命令/写删失败进命令行；各自的错误块都带下一步 |
| 2 | 命令失败只能在底部红字里读，错误文本与结果说明挤在同一行 | 命令行给 `kit::error_state_with_action` + 「关闭提示」（`redis-error-dismiss`），命令原文留在输入框可直接改；状态行只留结果说明，同一段错误不再两处堆叠 |
| 2.1 | `open_spec` 解析失败与「上一次读取还没结束」的忙时拒绝都直接写 `self.error`，`error_source` 停在上一轮的值——值区域可能顶着「读取键内容失败」的标题显示「上一条命令还在执行」 | 两处都改走 `show_error_from`：目标解析失败归 `Scan`（键列表），忙时拒绝归 `Command`（命令行）。`self.error` 的直接赋值只留在 `show_error_from` 内部 |
| 3 | 未选键右侧是 `kit::empty_state`（无操作）：标题还是「键空间」，没有下一步 | `kit::empty_state_with_action`：标题「还没有选择键」、说明复用 `redis.tab.pickKey`、主操作「新建键」（`redis-new-key-value`，窄窗口里会先切回键列表再打开表单） |
| 4 | 选中键但内容还没读回来时，值区域显示 `redis.tab.pickKey`（“选择一个键”），与实际状态矛盾 | 读取中 → `kit::skeleton_state(4, …)`；读取失败 → 值区域错误块 + 「重试读取」；只有既无内容又不忙时才给提示文字 |

## 3. 窄窗口重排（outcome 2，R4「三档宽度」）

| # | 本次之前 | 本次改动 |
| --- | --- | --- |
| 1 | 无视口宽度分支：左侧栏固定 `header_h * 6 + pane_pad`（紧凑约 262、标准约 300），640 宽窗口里值区域只剩约 340，TTL / 重命名两列表单被挤成一团 | `narrow = viewport_w < kit::NARROW_PAGE_WIDTH`（沿用底座断点，不自造常量）；窄窗口里键列表与值区域**交替占满整列**（主从式），不再互相挤压 |
| 2 | 两栏并排，窄窗口没有切换入口 | 窄窗口顶部 `redis-nav-strip`：`redis-nav-toggle` 在「键列表 / 键内容」之间切换，按钮文案指向要去的地方；选中键时自动转到值区域（`select_key` 内按 `viewport_w` 判断），不用再手动找 |
| 3 | 命令行属于右栏，窄窗口下若隐藏右栏就会一起消失 | 命令行固定在底部，与内容区同级；两条分支只切换 `row(左, column(值, 命令行))` 与 `column(切换条, 内容, 命令行)` |
| 4 | 窗口变宽后可能残留「用户手动收起左侧栏」的状态 | `keys_visible`：宽窗口无条件显示键列表，窄窗口才看 `nav_choice`（用户显式选择）与是否已选中键 |

窄窗口看键列表时，值区域（含 TTL / 重命名 / 删除 / 复制按钮）整块不渲染——同一时刻只有一栏，
这是主从式重排的取舍：宁可少一屏按钮，也不把两栏压成读不了的窄缝。命令行始终在底部，
写删命令与历史不受影响；危险确认条也在命令行区域，切换栏位不会把它挡住。

**没有改动的语义**（outcome 2 后半）：

- 浏览仍只走 `SCAN`（`load_keys` / `scan_from_start` / `merge_keys` 未改，`KEYS` 仍被 `command_risk` 拒绝）；
- TTL 仍要先确认（`save_ttl` → `Confirm::Ttl` → `run_confirm`），删除 / 重命名 / 写删仍先 `stage_mutation` 再确认；
- `FLUSHDB` / `FLUSHALL` / `SHUTDOWN` / 写删类命令仍走 `danger_prompt` 二次确认，危险块与「确认执行」按钮未动；
- 命令历史（`history_step` 上下键）、目标切换（`reset_target_state` 清空键/命令/确认/回复）、
  live 刷新（`Follow::Read` 与 `generation` 代次失效）都在原处。

## 4. 文案键必须真的存在

`i18n::text` 对未知键原样返回键名。本页新增 14 个键（`redis.list.busy`、`redis.empty.*`、`redis.noMatch.*`、
`redis.error.*`、`redis.nav.*`、`redis.retry.read`），中英都有值；
`every_redis_copy_key_used_in_the_source_exists_in_the_catalog` 反向扫描本文件里所有 `redis.*` / `common.*` 字面量，
逐个要求出现在 `i18n::catalog_keys()` 里，防止出现「界面上显示 redis.xxx」的旧问题。
