# T2 测试回执

环境：macOS，Rust 1.90+（仓库 `rust-toolchain` 默认），共享 target 目录
`~/.cache/cargo-target`（同批其它任务并行编译，命令可能等待 target 锁）。
所有命令在仓库根目录执行，依赖文件未改动（`src-tauri/Cargo.toml`、`Cargo.lock` 无变更）。

## 实际运行

| 命令 | 结果 |
| --- | --- |
| `cargo check --manifest-path src-tauri/Cargo.toml --all-targets --locked` | 通过（0 error） |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib -- connections::` | ok：59 passed；0 failed |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib -- app::views::connections` | ok：7 passed；0 failed |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib -- app::shell::tests` | ok：116 passed；0 failed；1 ignored（`mysql_lazy_navigation_latency_fixture`，需要 loopback 33067 的 `scripts/verify-catalog-mysql.py`，本任务未运行，**不计为通过**） |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked` | FAILED：895 passed；6 failed；7 ignored。6 个失败全部在并行任务的写入范围：`app::views::query::tests::*`（3 个，T6 查询错误元数据在途）、`app::views::structure::tests::*`（3 个，T9 表结构页面在途）。T2 范围内无失败。 |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | 0 error。T2 改动文件中仅剩 1 条 warning：`src/app/shell/model.rs:1288 clippy::question_mark`（`fn menu_key`，HEAD 已有，不属于本任务 diff）。T2 新增代码无 clippy warning。 |

## 逐用例（T2 新增 12 个）

`src-tauri/src/connections/tests.rs`（6）：

- `candidate_test_needs_no_saved_id_and_writes_nothing`
- `candidate_test_uses_the_edited_draft_instead_of_the_saved_row`
- `candidate_test_reuses_a_stored_password_only_when_the_draft_still_saves_it`
- `failed_candidate_test_leaves_no_trace_in_the_store`
- `recent_order_follows_successful_opens_only`
- `usage_table_migrates_onto_an_existing_config_database`

`src-tauri/src/app/views/connections/mod.rs`（3）：

- `recent_rows_follow_the_persisted_order_and_skip_unknown_ids`
- `failure_maps_to_the_field_the_user_should_fix`
- `draft_fingerprint_tracks_edits_and_secrets`

`src-tauri/src/app/shell/tests.rs`（3）：

- `connection_test_runs_on_the_unsaved_draft_and_keeps_it_on_failure`
- `stale_connection_test_result_does_not_overwrite_new_edits`
- `saving_a_connection_does_not_count_as_a_successful_open`

## 测试数据说明

- SQLite 用例使用经 sqlx 创建的真实空库（`sqlite_fixture` / `sqlite_file`）。首轮曾用任意字节当
  “数据库文件”，真实探测在 `ReadSchema` 阶段失败（`文件已打开，但没有读到 schema。`）——已改为真实库，
  失败原因保留在此，避免把它误当成“测试没生效”。
- 远程引擎失败路径使用脚本探针 `ScriptSsh`/`ScriptDb` 与 `MemoryStore`，不连网络、不依赖外部 fixture。
- 没有 `#[ignore]` 的新增用例；唯一被忽略的用例是上文那条既有 GPUI 延迟测试。

## 本轮未执行

- 原生窗口启动与截图：本环境无原生窗口/截屏通道，未生成连接弹窗、启动台或字段错误排版的 PNG。
- `style_literals_follow_design_tokens`：本任务未改任何 UI 样式字面量（`chrome.rs` 不在 T2 范围），
  该门禁由 Delivery Owner 在集成后的 candidate 上连同全量命令一并核对。
