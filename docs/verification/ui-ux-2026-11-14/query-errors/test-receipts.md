# 测试回执（T6）

运行环境：本机 macOS 共享工作区 `/Users/yanggeng/github/RillSQL`，Rust 单元测试与 GPUI headless 测试。
所有命令均带 `--locked`；Cargo 依赖未改动（`git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock` 退出码 0）。

## 复跑基线（先看这一节）

**`fc513e1` 单独 checkout 不可编译**：该提交的 `src-tauri/src/app/shell/tests.rs` 已带 T6 的 `StatementView` 字面量字段（`sent_sql` / `rewrite` / `rewrite_anchor`，各 2 处），而 `fc513e1` 的 `src-tauri/src/query/desk.rs` 还没有声明这三个字段，`cargo check --all-targets` 会直接 E0560。因此 **`fc513e1` 不能当作可编译的绿基线**。

- 可核对事实：`git show fc513e1:src-tauri/src/app/shell/tests.rs | grep -c sent_sql` = 2；`git show fc513e1:src-tauri/src/query/desk.rs | grep -c "pub sent_sql"` = 0。
- 实现期（提交前）的所有运行，实际环境是 **`fc513e1` 工作区叠加本任务未提交改动**，其代码树随后被提交为 `c1d0781`；`git diff --name-only fc513e1 c1d0781` 恰好只有 T6 的 8 个代码路径 + 5 份证据文档。
- 本文此前把这类运行写成“在 HEAD `fc513e1` 上复跑”，那是不成立的归属；下面把「提交前的实现期自检」与「绑定提交的干净基线」分开列，对外可引用的是后者。

## 干净基线复跑（绑定提交 61fcd18）

共享主工作区当时带有其它任务未提交的改动（`app/views/compare/mod.rs`、`app/views/diagram/mod.rs` 编译失败），无法在主工作区取得提交级基线。因此用 detached worktree 单独检出 `61fcd18` 复跑，与主工作区互不影响：

```
git worktree add --detach /tmp/t6-baseline-61fcd18 61fcd18
cd /tmp/t6-baseline-61fcd18
CARGO_TARGET_DIR=/tmp/t6-baseline-target cargo check  --manifest-path src-tauri/Cargo.toml --locked --all-targets
CARGO_TARGET_DIR=/tmp/t6-baseline-target cargo test   --manifest-path src-tauri/Cargo.toml --locked --lib -- query::
CARGO_TARGET_DIR=/tmp/t6-baseline-target cargo test   --manifest-path src-tauri/Cargo.toml --locked --lib
CARGO_TARGET_DIR=/tmp/t6-baseline-target cargo clippy --manifest-path src-tauri/Cargo.toml --locked --all-targets
```

`61fcd18` = T6 代码提交 `c1d0781` + T9 的 `33215ed` + 本文档 10 行；`git diff --name-only 33215ed 61fcd18 -- "*.rs"` 为空，代码树与 `33215ed` 相同。复跑后临时 target 目录已用 `cargo clean` 删除（7.8GiB）；主工作区未被改动，也没有新建或切换任何分支。detached worktree 目录 `/tmp/t6-baseline-61fcd18` 仍留在磁盘并登记在 `.git/worktrees`，因为本沙箱策略拒绝了 `git worktree remove`；需 Owner 执行一次 `git worktree remove /tmp/t6-baseline-61fcd18` 收尾（不影响主工作区与分支）。

| # | 命令（在 `61fcd18` 干净检出上运行） | 结果 |
| --- | --- | --- |
| A | `cargo check --all-targets` | 退出码 0，0 error |
| B | `cargo test --lib -- query::` | ok：153 passed / 0 failed / 1 ignored |
| C | `cargo test --lib` | ok：**903 passed / 0 failed / 7 ignored**（比实现期的 901 多 2 条，来自 `33215ed` 新增的 structure 用例） |
| D | `cargo clippy --all-targets` | 退出码 0，0 error；本次 34 条 crate 级 warning 中，本任务文件（`query/*`、`app/views/query/*`、`i18n/mod.rs`）占 0 条 |

## 实现期运行（提交前的共享工作区，如实标注环境）

| # | 命令 | 环境 | 结果 |
| --- | --- | --- | --- |
| 1 | `cargo test ... --lib query::` | `fc513e1` + T6 未提交改动（≡ `c1d0781` 代码树） | ok：153 passed / 0 failed / 1 ignored |
| 2 | `cargo test ... --lib app::views::query` | 同上 | ok：38 passed / 0 failed |
| 3 | `cargo test ... --lib both_locales_translate_every_catalog_key` | 同上 | ok：1 passed |
| 4 | `cargo test ... --lib english_copy_has_no_chinese_and_query_titles_follow_locale` | 同上 | ok：1 passed |
| 5 | `cargo test --manifest-path src-tauri/Cargo.toml --locked` | 同上，另叠加 T9 当时未提交的 structure 改动 | 901 passed / 0 failed / 7 ignored；中途一次出现 3 个 `app::views::structure::tests` 失败，属 T9 正在改该文件的瞬时状态，本任务未触碰该文件 |
| 6 | `cargo clippy --all-targets --locked` | 同上 | 退出码 0；本任务文件 0 warning（改动前两条 dead_code 已用 `#[cfg(test)]` 收敛） |
| 7 | `git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock` | 同上 | 退出码 0（Cargo 依赖未变） |

上表只说明实现期的自检过程，不作为对外通过证据。

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

- 本任务在写 `src-tauri/src/query/tests.rs` 时，有一次整文件写入收到的是非字符串值，导致该文件被写成 `[object Object]`（81476 → 5894 字节），整 crate 测试目标一度编译失败，并阻塞了同批次其它任务的编译。
- 恢复方式：`git show HEAD:src-tauri/src/query/tests.rs > src-tauri/src/query/tests.rs` 回到基线，再用精确字符串编辑重新施加本任务的全部用例（新增 6 个执行层/桌面层用例、更新 2 个驱动替身与 1 个结构断言），没有删改任何既有用例。恢复后字节数 97807（基线 81476 + 本任务内容），该内容随后提交为 `c1d0781`。
- 此后所有整文件写入只以字符串值进行，做法改为“先写临时文件再 `cat >>`”或精确 `Edit`。

## ignored 用例

- `query::` 过滤下 1 个 ignored：需要真实 MySQL/MariaDB fixture 的驱动用例，本轮未启动数据库容器，不计作通过。

## 未取得项

- 原生窗口与三档宽度 PNG 截图：未采集（理由见 `widths-1360-900-640.md`），由 Delivery Owner 在集成 candidate 上补齐。
- 7 条 ignored 均为需要真实数据库 fixture（MySQL/PostgreSQL/MariaDB 或平台相关）的用例，本轮未启动容器，不计作通过。
