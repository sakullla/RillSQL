# T2 行为对应：草稿测试与最近连接的服务基础

## Outcome 1 — 新建/修改草稿无需保存即可测试，请求不再被 selected id 的旧配置替代

| 项 | 内容 |
| --- | --- |
| 服务入口 | `Service::test_candidate(draft, input)`（`src-tauri/src/connections/service.rs`）：先 `model::normalize` 出候选配置，再用 `open=false` 调用 `run_attempt`。全程不读 `store.get(id)` 作为被测配置，不写 `connections` 表，不建会话。 |
| View 入口 | `connections::test_row(services, draft, secrets)`（原 `test_row(id)` 改为接收草稿，保留 `src/lib.rs` 的“连接页经服务层、不经 Tauri”守卫测试语义）。 |
| 任务链 | `job_request` 里的 `draft`/`secrets` 快照 → `PendingJob::TestConnection` → `test_row`；`conn-test` 不再因为 `editor.id != selected` 去 `edit_selected()` 覆盖屏幕上的草稿。 |
| 已保存机密 | 只有草稿仍勾选“保存密码”（`draft.save_password`）且该 id 在凭据库有密码时才沿用；与 `Service::save` 的 `password_saved` 判定一致，取消保存密码就必须重新输入。 |
| 用例 | `candidate_test_needs_no_saved_id_and_writes_nothing`、`candidate_test_uses_the_edited_draft_instead_of_the_saved_row`、`candidate_test_reuses_a_stored_password_only_when_the_draft_still_saves_it`、`connection_test_runs_on_the_unsaved_draft_and_keeps_it_on_failure` |

取舍：测试成功不写 `LinkStatus::Connected`——测试只证明这份配置能连上，不等于已经建立会话；
状态写入也改成按“屏幕草稿的 id”，不再按列表 `selected`，避免新建草稿的测试结果标记到别的连接行上。

## Outcome 2 — 失败保留 draft/secrets，`FieldIssue` 关联到具体字段，过期结果不覆盖新编辑

| 项 | 内容 |
| --- | --- |
| 字段级问题 | `FieldIssue` 新增 `detail: String`（失败原文）与 `new`/`with_detail` 构造；`validate_draft` 全部走 `new`。 |
| 失败映射 | `field_issue_for_failure(report, draft)`：取第一个 `Err`/`Wait` 阶段 → `file_path`（OpenFile/ReadSchema/ForeignKeys）、`host`（Network/ServerVersion）、`ca_file`（Tls 且 CA 选文件，否则 `host`）、`password`（`conn.password_required`）、`username`（其它数据库认证失败）、`ssh_password`/`ssh_host`（SSH 认证/身份/隧道）。通用文案固定 `connections.status.error`，原文留在 `detail`。 |
| 过期隔离 | `draft_fingerprint(editor, secrets)` 对草稿 JSON 与三项本次机密取内容哈希；`job_request` 时算一次随 `JobDone::Test` 回传，`ConnectionPage::note_test` 在写入前比对当前指纹，不一致就只收起“正在测试”，不写状态/消息/字段错误。表单关闭（`editor = None`）与改任意字段都会改变指纹。 |
| 编辑即清错 | `ConnectionPage::clear_issue(field)` + `model.set_field` 的字段映射（`issue_field`）：改 host/port/file_path/ssh_*/密码后该字段的旧结论立即消失。 |
| 校验前置 | `conn-test` 先跑 `validate()`，字段本身不合法时不发一次必然失败的探测，直接把错误留在字段旁。 |
| 用例 | `connection_test_runs_on_the_unsaved_draft_and_keeps_it_on_failure`（失败后草稿/机密仍在、`issues[0].field == "file_path"`、改字段后清错、原地重测成功）、`stale_connection_test_result_does_not_overwrite_new_edits`（改字段与关表单两种迟到结果）、`failure_maps_to_the_field_the_user_should_fix`、`draft_fingerprint_tracks_edits_and_secrets`、`failed_candidate_test_leaves_no_trace_in_the_store` |

## Outcome 3 — 只有成功打开才更新最近使用；保存不连接；连接定义与机密边界不变

| 项 | 内容 |
| --- | --- |
| 存储 | `connections.sqlite` 新增独立的 `connection_usage(id, last_used_at)` 表（`CREATE TABLE IF NOT EXISTS`，存量库只多一张空表）。**不**给 `SavedConnection` 加字段，因此导出/导入、凭据模型与其它视图的 `SavedConnection` 字面量都不受影响。 |
| 写入点 | 只在 `run_attempt` 真正把新会话插入 `sessions` 之后调 `Store::touch_last_used`。保存（`Service::save`）、测试（`test_candidate`，`open=false`）都不写这一行。 |
| 排序 | `Store::last_used()` 按 `last_used_at DESC, id` 返回；`touch_last_used` 取 `max(now_ms, 当前最大 + 1)`，同一毫秒内连续打开也能分出先后，排序不依赖插入顺序。`Service::recent_ids(limit)` 过滤已删除/未知 id，`ConnectionPage::recent(order, limit)` 按该顺序取行，不认识的 id 跳过——不用“列表前 6 条”冒充最近。 |
| 删除 | `Store::delete` 同时删除用量行；断开连接（`disconnect`）不清除历史使用时间。 |
| 边界 | 机密仍只经 `SecretStore`；`test_candidate` 只读凭据库、从不写；配置表不因测试或用量变化。 |
| 用例 | `saving_a_connection_does_not_count_as_a_successful_open`、`recent_order_follows_successful_opens_only`（保存→空；open A→A；open B→B,A；再 open A→A,B；限长；断开不改序；删除清用量）、`recent_rows_follow_the_persisted_order_and_skip_unknown_ids`、`usage_table_migrates_onto_an_existing_config_database` |

## Outcome 4 — 回归覆盖清单

| 场景 | 用例 |
| --- | --- |
| 全新草稿可测 | `candidate_test_needs_no_saved_id_and_writes_nothing`、`connection_test_runs_on_the_unsaved_draft_and_keeps_it_on_failure` |
| 编辑旧配置测的是草稿 | `candidate_test_uses_the_edited_draft_instead_of_the_saved_row`、`candidate_test_reuses_a_stored_password_only_when_the_draft_still_saves_it` |
| 失败保留草稿/机密 + 字段关联 | `connection_test_runs_on_the_unsaved_draft_and_keeps_it_on_failure`、`failure_maps_to_the_field_the_user_should_fix` |
| 旧任务竞争 | `stale_connection_test_result_does_not_overwrite_new_edits`、`draft_fingerprint_tracks_edits_and_secrets` |
| 最近使用排序 | `recent_order_follows_successful_opens_only`、`saving_a_connection_does_not_count_as_a_successful_open`、`recent_rows_follow_the_persisted_order_and_skip_unknown_ids` |
| Store 迁移 | `usage_table_migrates_onto_an_existing_config_database`（删表模拟旧库 → 重开补表、已有连接不丢、可再迁移） |

## 已知限制 / 未解决事实

1. 字段错误目前仍渲染在编辑器下方的提示条（`chrome.rs`，T3 范围）。模型层已带上字段名与原文，
   T3 换成“字段旁”只需读 `issue.field` / `issue.detail`，无需再改服务层。
2. 未保存草稿测试遇到“等待确认 SSH 主机密钥”时，只有草稿带已保存 id 才会登记待确认项
   （`confirm_host_key` 需要一条已保存连接来定位主机密钥行）；纯新草稿的该状态目前只能看到失败提示。
3. 自动重连成功与 `focus_database` 改连成功也走同一条“插入会话 → 记最近使用”的路径，因此也会刷新时间；
   这是“成功打开即最近使用”的一致结果，不是保存副作用。
4. 本任务未取得原生窗口/截图证据，见 `README.md`。
