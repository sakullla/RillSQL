# 测试回执（T6）

运行环境：本机 macOS 工作区 `/Users/yanggeng/github/RillSQL`，Rust 单元测试与 GPUI headless 测试。
所有命令均带 `--locked`，Cargo 依赖未改动（`git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock` 退出码 0）。

## 本轮实际运行

| # | 命令 | 结果 |
| --- | --- | --- |
| 1 | `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib query::` | ok：153 passed / 0 failed / 1 ignored |
| 2 | `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib app::views::query` | ok：38 passed / 0 failed |
| 3 | `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib both_locales_translate_every_catalog_key` | ok：1 passed |
| 4 | `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib english_copy_has_no_chinese_and_query_titles_follow_locale` | ok：1 passed |
| 5 | `cargo test --manifest-path src-tauri/Cargo.toml --locked` | 最终：**901 passed / 0 failed / 7 ignored**（lib 901 + 其余目标全绿）。中途一次运行曾出现 3 个 `app::views::structure::tests` 失败，那是对应文件正被同批次 T9 改写的瞬时状态，本任务未触碰该文件；T9 落地后同一命令转绿 |
| 6 | `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | 退出码 0；本任务文件（`query/*`、`app/views/query/*`、`i18n/mod.rs`）无新增 warning（改动前曾出现两条 dead_code，已用 `#[cfg(test)]` 收敛） |
| 7 | `git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock` | 退出码 0（Cargo 依赖未变） |
| 8 | `cargo check --manifest-path src-tauri/Cargo.toml --locked --all-targets` | 退出码 0，0 error（在 HEAD `fc513e1` 上复跑） |
| 9 | `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib -- query::` | ok：153 passed / 0 failed / 1 ignored（在 HEAD `fc513e1` 上复跑） |
| 10 | `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib` | ok：901 passed / 0 failed / 7 ignored（在 HEAD `fc513e1` 上复跑） |

## 本任务新增/更新的用例

执行层（`src-tauri/src/query/tests.rs`）

- `auto_limit_rewrite_reports_what_it_appended_or_replaced`：追加/替换 LIMIT、手写 LIMIT 对照、上限 10000+1、TOP 追加与替换、FETCH 改数字、不返回行的语句不改写；并逐条与 `cap_result_sql` 对照证明规则未变。
- `batch_carries_the_statement_it_sent_and_the_rewrite_source`：成功/失败/skipped 三条语句分别带回 `sql`、`sent_sql`、`rewrite`；skipped 没有实际语句。
- `hand_written_limit_keeps_the_sent_sql_and_adds_no_note`：手写 LIMIT 未被改写时不产生改写来源。
- `rewritten_error_maps_back_to_the_draft_statement_boundary`：改写位置映射到草稿里的插入边界（语句末尾分号前），SQLite 无位置时整条语句标错，有行号时按行标错。
- `a_stale_run_cannot_write_rewrite_metadata_onto_newer_results`：过期 ticket 的改写信息不覆盖新结果。
- `the_same_long_error_is_logged_once_in_messages`：整次执行记录与语句记录同文时不重复堆，不同失败仍各记一条。
- `mysql_batch_checks_out_once_and_does_not_reset_on_return`（更新）：新增断言要求 `postgres_run` 不再自己调用 `cap_result_sql`，改写点只剩 `LiveDriver::run` 一处。

界面层（`src-tauri/src/app/views/query/mod.rs` 测试模块）

- `an_auto_rewritten_error_explains_the_rewrite_and_shows_the_sent_sql`：改写说明 + 实际语句 + 头部只有「失败」+ 定位选中整条语句。
- `a_hand_written_limit_error_gets_no_rewrite_note`：手写 LIMIT 报错无改写区块，消息页也不附加。
- `the_message_page_shows_the_rewrite_once_without_stacking_the_same_error`：消息页一条记录、改写说明与实际语句各一次、长错误只出现一次。
- `rewritten_error_evidence_holds_at_1360_900_and_640`：三档宽度下的同一场景（见 `widths-1360-900-640.md`）。
- `a_failed_statement_can_be_jumped_to_and_later_statements_read_as_skipped`（更新）：结果头断言从“包含长错误”改为“只有短状态、原文在语句面板里”。

## 工作区事故与恢复（如实记录）

- 本任务在写 `src-tauri/src/query/tests.rs` 时，有一次整文件写入收到的是非字符串值，导致该文件被写成 `[object Object]`（81476 → 5894 字节），整 crate 测试目标一度编译失败。
- 恢复方式：`git show HEAD:src-tauri/src/query/tests.rs > src-tauri/src/query/tests.rs` 回到基线，再用精确字符串编辑重新施加本任务的全部用例（新增 6 个执行层/桌面层用例、更新 2 个驱动替身与 1 个结构断言），没有删改任何既有用例。
- 恢复后字节数：97807（基线 81476 + 本任务新增/更新内容）。此后所有整文件写入只以字符串值进行，改为“先写临时文件再 `cat >>`”或精确 `Edit`。
- 当前 HEAD 为 `fc513e1`（T2 的连接测试提交）。在该 HEAD 上复跑：`cargo check --all-targets` 退出码 0；`cargo test --lib -- query::` 153 passed / 1 ignored；`cargo test --lib` 901 passed / 0 failed / 7 ignored。

## ignored 用例

- `query::` 过滤下 1 个 ignored：需要真实 MySQL/MariaDB fixture 的驱动用例，本轮未启动数据库容器，不计作通过。

## 未取得项

- 原生窗口与三档宽度 PNG 截图：未采集（理由见 `widths-1360-900-640.md`），由 Delivery Owner 在集成 candidate 上补齐。
- 7 条 ignored 均为需要真实数据库 fixture（MySQL/PostgreSQL/MariaDB 或平台相关）的用例，本轮未启动容器，不计作通过。
