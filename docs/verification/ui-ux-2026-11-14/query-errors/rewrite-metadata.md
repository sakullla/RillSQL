# 实际执行 SQL 与自动改写元数据（T6 · Outcome 1/3）

## 一句话

执行结果现在同时带「用户写的原文」和「真正发出去的语句」，外加这一处自动改写的种类、插入点和被插入文本；改写规则、TOP/LIMIT/FETCH 的判定与 1..=10000 读取上限一个字都没动。

## 数据结构

| 位置 | 字段 | 含义 |
| --- | --- | --- |
| `query::exec::AutoRewrite` | `kind` / `at` / `replaced` / `added` | 改写种类（limit/fetch/top）、原语句里的插入点或被替换数字起点、被替换长度、实际插入的文本（含补的空格） |
| `query::exec::CappedSql` | `sql` / `rewrite` | 实际会发出去的文本与它的改写来源 |
| `query::exec::StatementReport` | `sql` / `sent_sql` / `rewrite` | 语句级报告：原文、实际语句、改写来源 |
| `query::commands::StatementDto` | `sentSql` / `rewrite` | 传给界面的安全字段；没有改写时 `sentSql` 为 null，不重复一份与原文相同的文本 |
| `query::desk::StatementView` | `sent_sql` / `rewrite` / `rewrite_anchor` | 界面状态：实际语句、改写来源、改写位置在草稿里的字节偏移（后者仅作说明，不参与定位） |
| `query::desk::LogEntry` | `sent_sql` / `rewrite` | 消息页每条记录同样带改写说明，不需要回查结果 |

## 改写点只剩一处

- `exec::LiveDriver::run` 在拿会话之前算一次 `cap_result_sql_detail(&self.engine, sql, limit)`，把结果文本交给驱动，并把同一个 `CappedSql` 随 `DriverReport` 交回 `run_batch`。
- 四个驱动（sqlite/mysql/postgres/sqlserver）不再自己改写：收到什么就发什么，因此“报告的语句”与“真发出去的语句”不可能是两份不同的文本。
- `cap_result_sql` 保留为只返回文本的测试用入口，`auto_limit_rewrite_reports_what_it_appended_or_replaced` 用它逐条对照 `cap_result_sql_detail(...).sql`，证明规则未变；`mysql_batch_checks_out_once_and_does_not_reset_on_return` 里新增的断言要求 `postgres_run` 不再出现 `cap_result_sql`。

## 改写种类与规则（保持原样）

| 场景 | 实际发出的文本 | 改写来源 |
| --- | --- | --- |
| 追加 LIMIT | `SELECT * FROM logs WHERE LIMIT 1001` | `kind=limit`, `at=22`, `replaced=0`, `added=" LIMIT 1001"` |
| 改小 LIMIT 数字 | `SELECT a FROM t LIMIT 101 OFFSET 0` | `kind=limit`, `at=21`, `replaced=4`, `added="101"` |
| 手写 LIMIT 未超上限 | 原语句 | 无（`rewrite=None`） |
| SQL Server 追加 TOP | `SELECT TOP (11) [limit] FROM t` | `kind=top`, `at=6`, `added=" TOP (11)"` |
| SQL Server 已有 TOP | `SELECT TOP (11) id FROM t` | `kind=top`, `replaced=4`, `added="11"` |
| 已有 FETCH NEXT | `... FETCH NEXT 101 ROWS ONLY` | `kind=fetch`, `added="101"` |
| 不返回行的语句（UPDATE/EXEC 等） | 原语句 | 无 |

读取上限仍是 `clamp_limit` 的 1..=10000，探测行数仍是 `cap + 1`（`SELECT 1` 在 10000 上限下发出 `LIMIT 10001`）。`unsafe_limit`（LIMIT @n、LIMIT (?）、`INTO`、SQL Server 的 WITH 开头语句依旧不改写。

## 定位映射不变量

1. **错误位置永远相对草稿原文**。PostgreSQL 的字符位置先在发出的文本里算出，再用 `map_rewritten_char` 映射回原文；MySQL/SQL Server 的行号不受改写影响，因为改写只在同一行内插入或替换数字，从不增加行数。`desk::place_statements` 与 `error_mark` 的既有换算因此不需要改动。
2. **`rewrite_anchor` 只是改写位置元数据，不是定位机制**：`rewrite_anchor = 语句起点 + rewrite.at`（越界时停在语句末尾），用途是说明"改写发生在原草稿的哪个字节"。它没有生产消费方——定位与错误标记一律由 `error_mark` 决定，界面不会拿这个偏移去跳转或高亮。`rewritten_error_maps_back_to_the_draft_statement_boundary` 断言它落在草稿里这条语句末尾分号之前，测的是这段换算本身。
3. **不伪造精确位置**：驱动没有偏移（SQLite）时 `error_mark = ErrorMark::Statement`，定位与错误标记都用整条语句的范围；`rewrite_anchor` 既不参与定位，也不当作错误位置。
4. **草稿不被替换**：编辑器里始终是用户自己写的语句，改写文本只出现在结果面板与消息页的只读展示里。

## 过期结果

`a_stale_run_cannot_write_rewrite_metadata_onto_newer_results`：同一标签先后两次执行，旧 ticket 的 `sent_sql`（LIMIT 101）晚到也不会覆盖新结果（LIMIT 201），token 不匹配时整批丢弃。
