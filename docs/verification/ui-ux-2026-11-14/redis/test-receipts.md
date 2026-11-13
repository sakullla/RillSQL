# 测试与命令回执（T14）

全部命令在仓库工作区（HEAD `df79f24` + 本任务改动）上于 2026-11-14 实际执行。
本文件如实记录修复轮次：第一版新增用例跑出 5 条失败，全部是**测试自身的假设错误**（不是产品缺陷），
修正后同批用例全绿，详见第 6 节。

## 1. 正式命令

| 命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked app::views::redis` | 通过（T14-A2 修复后复跑）：**15 passed / 0 failed**（959 filtered），含 8 个本任务新增用例与 3 个真实 Redis 夹具用例 |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked` | 通过：**966 passed / 0 failed / 7 ignored**（42.78s，exit 0）。这是第一轮交付时的回执；T14-A2 修复后按 Owner 要求只复跑本模块，未再跑全量 |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked style_literals_follow_design_tokens` | 通过：`ui::kit::token_tests::style_literals_follow_design_tokens ... ok`（1 passed） |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | 通过（exit 0）。`src/app/views/redis/states.rs` 0 条；`redis/mod.rs` 只剩 HEAD 已有的 `Field` 枚举 `enum_variant_names` 与 `EntryIndex` never constructed，本任务未新增 |
| `git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock` | 通过（exit 0），Cargo 依赖未变 |

按计划，这三条由 Delivery Owner 在集成后的同一 candidate 上重跑；本回执只证明本任务范围内通过。

## 2. 本任务新增用例（`app::views::redis::states`）

| 用例 | 覆盖 | 证据类型 |
| --- | --- | --- |
| `key_list_state_keeps_busy_error_no_match_and_empty_apart` | 四态判定表的每一行（六个输入），含「加载优先于错误」「有键但过滤掉是无命中而非无数据」「下一页读取时行还在」「带模式 0 命中是无命中」「没读完不宣布空库」 | 源码级 |
| `empty_state_needs_a_finished_scan_and_no_pattern` | 带模式扫出 0 个键 → `no-match-state` + `redis-clear-filter`，且没有空态/「新建键」；第一页 0 个键但游标没读完 → 不画空态也不画无命中，页脚 `redis-more` 仍在 | headless 布局级 |
| `key_list_states_render_with_a_next_step` | 900×900 下：无数据 → `empty-state` + `state-action`；首屏读取 → `skeleton-state` 且键列表没有空态；类型过滤无命中 → `no-match-state`；SCAN 失败 → `error-state` + `redis-retry-scan`；有行再读 → `inline-loading` 且行还在 | headless 布局级 |
| `narrow_window_swaps_the_key_list_and_the_value_area` | 640×720：选中键后只渲染值区域、切换条与按钮都在窗口内；点切换后键列表占满整列、按钮文案变成「键内容」；选中键自动转值区域；1100 宽时两栏并存且没有切换条 | headless 布局级 |
| `failures_land_in_the_context_that_produced_them` | 读键内容失败 → 值区域 `error-state` + `redis-retry-read`；命令失败 → 命令行 `error-state` + `redis-error-dismiss`，且状态行仍在 | headless 布局级 |
| `state_actions_drive_the_model` | `redis-clear-filter` 真的清模式与类型过滤（且不动选中键）；`redis-error-dismiss` 清错误；`keys_visible` 在窄/宽窗口的三条规则 | 源码级（模型） |
| `redis_pane_holds_both_themes_both_densities_and_three_widths` | 浅/深色 × 紧凑/标准 × 1360/900/640（12 个组合）：面板宽不越出窗口、宽窗口键行高 = `tree_row_h + 2`、切换条只在窄窗口、窄窗口值区域在位 | headless 布局级（缺像素） |
| `every_redis_copy_key_used_in_the_source_exists_in_the_catalog` | 反向扫描 `redis/mod.rs` 里 35 个 `redis.* / common.*` 字面量，要求都出现在 `i18n::catalog_keys()` | 源码级（防回归扫描） |

## 3. 回归相关的既有用例（全部通过）

- `tests::switching_redis_target_clears_key_command_and_confirmation`：换目标清空键、命令、TTL、重命名与确认；
- `tests::config_set_ttl_score_and_history_stay_local`：`CONFIG SET` 必须先确认、TTL 先确认且草稿保留、非法分值不进确认、命令历史上下键、控制台开关；
- `tests::command_parsing_ttl_and_keys_merge_stay_local`：命令切分、TTL 解析、SCAN 合并、危险提示；
- `live::rendered_controls_refuse_keys_and_keep_unconfirmed_input`：真实夹具上「按钮文案 + `KEYS` 被拒 + `FLUSHALL` 需确认 + 取消保留命令」；
- `live::live_redis_scans_filters_reads_ttl_rename_delete_and_confirms`、`live::confirm_keeps_the_target_opened_with...`、
  `live::sidebar_redis_button_opens_the_browser`：真实 Redis 夹具上的扫描、过滤、读值、TTL、重命名、删除与确认（本机夹具可达，未跳过）。

这三个 live 用例本次真的跑到了夹具（`test ... ok`，不是 ignored），因此窄宽重排没有破坏 live 行为与危险确认。

## 4. 相邻边界

- 全量 `cargo test` 覆盖 `ui::kit`（三态排版、token 扫描）与其余视图，966 passed / 0 failed，未见因本页改动导致的回归；
- `i18n::tests::both_locales_translate_every_catalog_key` 通过：新增的 14 个键中英都有值；
- 未改动 `src-tauri/src/app/views/redis/live.rs`（其 3 条夹具用例照常通过）。

## 5. 未执行 / 不计作通过

- **原生 `rillsql` 启动与截图**：本环境没有原生窗口与截屏通道，未执行。浅/深色 × 紧凑/标准 × 三档宽度的像素证据缺，
  需 Delivery 在集成 candidate 上补；本目录不用历史截图替代。
- **7 个 ignored 用例**：既有 fixture/基准用例，与 T1 回执列出的同一批，本轮未运行，不计作通过。
- **MySQL/PostgreSQL/MariaDB 容器**：本任务不涉及关系库连接，未启动。

## 6. 修复轮次（如实记录）

第一版新增用例跑出 5 条失败，逐条定位后确认都是**测试假设错误或测试基础设施用法错误**，产品代码只补了 `test_support` 标记：

| 失败用例 | 原因 | 修正 |
| --- | --- | --- |
| `every_redis_copy_key_used_in_the_source_exists_in_the_catalog` | 阈值写成 ≥ 40，实际扫描到 35 个键 | 阈值改为 ≥ 30（仍能证明扫描生效） |
| `state_actions_drive_the_model` | 注入的 `plant_key` 已选中键，却断言「窄窗口未选键先看键列表」 | 先断言已选中时看值区域，再清空 `selected` 断言未选键时看键列表 |
| `narrow_window_swaps_...`、`redis_pane_holds_...` | `find` 只匹配带 `test_support()` 的可观测节点；`redis-keys` / `redis-value` / `redis-pane` 只有 `.id()`，只作为祖先路径出现 | 给三个容器加 `.test_support()`（与 `structure-pane` / `structure-body` 同一写法） |
| `key_list_states_render_with_a_next_step` | 未选键时值区域也画空态与 `state-action`，同名 id 歧义；`寻找 empty-state` 还会命中值区域那块 | 断言按 `within("redis-keys")` 作用域取，避免跨区域误判 |

修正后同一批用例一次通过；产品代码的行为在修正前后没有变化（除了为了让无头测试能定位容器而加的 `test_support` 标记，它不影响布局与交互）。

## 7. 第二轮修复（T14-A2，审查意见）

审查提出 1 条阻塞（P2）与 1 条 P3，均已修复：

| 意见 | 事实 | 修法 | 用例 |
| --- | --- | --- | --- |
| P2：`list_state` 没有「模式条件成立」与「SCAN 已读完」两个输入 | 模式框里填一个匹配不到的前缀时 SCAN 在服务端就过滤成 0，`total == 0` 被判成 Empty——界面会说「这个库还没有键」并给主操作「新建键」，而真正的下一步「清除筛选」只对类型过滤/收起前缀生效；另外第一页 0 个键但游标没读完也会宣布空库，与页脚「再读取一批」矛盾 | `list_state(busy, scan_failed, total, visible, pattern_active, done)`：`total == 0 && pattern_active` → NoMatch（主操作复用 `redis-clear-filter`，同时清模式与类型过滤）；`total == 0 && !done` → 不画状态块，保留页脚「再读取一批（SCAN）」；只有 `total == 0 && done && !pattern_active` 才是 Empty | `key_list_state_keeps_busy_error_no_match_and_empty_apart`（源码级）、`empty_state_needs_a_finished_scan_and_no_pattern`（渲染级） |
| P3：`open_spec` 解析失败与忙时拒绝直接写 `self.error`，`error_source` 停在上一轮的值 | 值区域会顶着「读取键内容失败」的标题显示「上一条命令还在执行」 | 两处都改走 `show_error_from`：目标解析失败 → `ErrorSource::Scan`（键列表），忙时拒绝 → `ErrorSource::Command`（命令行）；`self.error` 的直接赋值只留在 `show_error_from` 内部（现在用 `grep "self.error = "` 只能命中那一处） | 既有 `failures_land_in_the_context_that_produced_them` 覆盖三类落点；`state_actions_drive_the_model` 覆盖命令/扫描来源迁移 |

复跑：`cargo test --manifest-path src-tauri/Cargo.toml --locked app::views::redis` → **15 passed / 0 failed**（959 filtered）；
`cargo clippy --all-targets --locked` → exit 0，`redis/states.rs` 0 条告警。
按 Owner 要求**未复跑全量**（同期有其它批次在写），全量回执仍是第 1 节第一轮那次 966 passed 的记录。

