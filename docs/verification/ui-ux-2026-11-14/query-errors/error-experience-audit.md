# 查询错误体验审计（T6 · Outcome 2/3）

对照 R2 第 3 条与 S8.5/S12.5，逐条记录「源码入口—偏差—修复结果—验证材料」。探索阶段的记录（01-exploration.md 第 2 节）作为基线，本表是当前 candidate 的处置结果。

## 1. 自动改写导致的报错没有解释，也看不到改写后的语句

- 源码入口：`query/exec.rs`（`StatementReport` 只有原文）、`query/commands.rs`（`StatementDto` 同）、`app/views/query/mod.rs`（`statement_outcome`、`failure_status`）、`app/views/query/log.rs`（`LogPanel`）。
- 偏差：执行层知道这条语句被追加/改大过读取上限，却只把用户原文和驱动错误交上去；界面只能显示 `near "LIMIT": syntax error`，用户无法知道自己没写 LIMIT，也无法核对真正执行了什么。
- 修复：
  - 执行层把实际发出的语句与改写来源随结果带回（见 `rewrite-metadata.md`）。
  - 失败的语句在语句面板里多一段：一句「执行前被自动改写（追加 LIMIT 1001）…」+「实际执行的语句」+ 等宽的改写后文本（`query-rewrite` / `query-rewrite-note` / `query-rewrite-sql`）。
  - 消息页同一失败记录下也补同样的两句（`query-log-rewrite-N` / `query-log-sent-N`），条目可读名称里也包含改写说明，读屏能听到。
- 验证材料：`an_auto_rewritten_error_explains_the_rewrite_and_shows_the_sent_sql`、`the_message_page_shows_the_rewrite_once_without_stacking_the_same_error`、`batch_carries_the_statement_it_sent_and_the_rewrite_source`。

## 2. 手写 LIMIT 不该被附上改写说明

- 源码入口：`exec::cap_result_sql_detail`（`limit.value <= probe` 时原样返回，`rewrite=None`）、`statement_outcome` 的 `when_some(failed { rewrite_block })`、`log::LogPanel` 的 `failed.then(rewrite)`。
- 偏差：无改写信息可用时容易顺手给所有失败都加一句“可能被改写”。本轮用“只有真改写过才有 `rewrite`”把两种报错分开。
- 修复：`rewrite` 为 `None` 时不渲染任何改写区块，消息页也不加行。
- 验证材料：`auto_limit_rewrite_reports_what_it_appended_or_replaced`（手写 LIMIT 10 / 上限 100 时 `rewrite=None`）、`a_hand_written_limit_error_gets_no_rewrite_note`、`hand_written_limit_keeps_the_sent_sql_and_adds_no_note`。

## 3. 同一段长错误在结果头与消息面板重复堆叠

- 源码入口：`app/views/query/mod.rs::failure_status`（结果头）、`statement_outcome`（语句面板）、`query/desk.rs::stop_run/cancel`（整次执行记录）。
- 偏差（修复前）：结果头把整段驱动错误截到 360px 显示一遍，下面的语句面板再把同一段完整显示一遍；整次执行的失败记录还可能和语句记录写同一段文本。
- 修复：
  - 结果头只显示短状态「失败」+ 定位按钮；完整原文仍作为悬停提示保留（不额外在页面上重复）。
  - 完整错误只在语句面板（`query-error-message`）或消息页条目里各出现一次；两者不同时可见。
  - 新增 `push_run_log`：整次执行记录与已有语句记录文本相同时不重复入列。
- 验证材料：`a_failed_statement_can_be_jumped_to_and_later_statements_read_as_skipped`（头部标签为“失败”，面板里才是原文）、`the_same_long_error_is_logged_once_in_messages`（同段错误只 1 条，不同失败仍记 2 条）。

## 4. 定位按钮与错误标记要一致，且不伪造精确位置

- 源码入口：`app/views/query/mod.rs::error_ranges` / `error_marks` / `jump_to_error` / `jump_to_failure` / `sync_error_marks`，`query/desk.rs::error_mark` / `place_statements`。
- 偏差：改写后如果拿“实际语句里的位置”直接标草稿，会落到错误的字符上；SQLite 没有位置时也不能编一个。
- 修复：位置一律换算回原文（PostgreSQL 用 `map_rewritten_char`，行号不受改写影响），改写位置单独存为 `rewrite_anchor`；没有驱动位置时 `ErrorMark::Statement` 标整条语句。跳转、错误标记、悬停原文三者都读同一份 `error_ranges`。
- 验证材料：`rewritten_error_maps_back_to_the_draft_statement_boundary`（anchor 落在语句末尾分号前、`error_mark=Statement`；有行号时 `ErrorMark::Line(1)`）、`rewritten_error_evidence_holds_at_1360_900_and_640`（点击定位后选中的正是那条失败语句）。

## 5. 成功 / 失败 / skipped 与过期结果

- 成功语句：`sent_sql` 与 `rewrite` 都带回，但不加任何说明文字；成功语句本身不因“追加过 LIMIT”而加噪声。
- 失败语句：`sql` 原文、`sent_sql` 实际语句、`message` 驱动原文三者都在。
- skipped：`sent_sql` 为空、`rewrite=None`（没有发送，也就没有改写）。
- 过期结果：旧 ticket 的结果整批丢弃，不会把旧的改写说明写到新结果上。
- 验证材料：`batch_carries_the_statement_it_sent_and_the_rewrite_source`、`a_stale_run_cannot_write_rewrite_metadata_onto_newer_results`。

## 未解决/留给其他任务

- 结果表里被保留前一批行（失败但沿用旧列）时，语句面板不渲染，改写说明只在消息页与悬停里可见。这是既有结果区结构决定的，本轮不改结果区布局（属 T7 网格范围）。
- `connections/`、`app/shell/*`、`app/model.rs` 未改动；本任务不需要其中的接口变化。
- 本任务为新增文案改了 `src-tauri/src/i18n/mod.rs`（追加 7 条键，未改任何既有键），该文件不在 Recipe 列出的写入范围内，属直接消费者必需的相邻改动，已在结果里申报。
