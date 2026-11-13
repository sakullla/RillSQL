# 测试回执（T11）

运行环境：本机 macOS 工作区 `/Users/yanggeng/github/RillSQL`，Rust 1.90 + GPUI 无头测试。
命令均带 `--locked`；本任务没有改 `Cargo.toml` / `Cargo.lock`
（`git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock` 退出码 0）。

## 本轮实际运行

| # | 命令 | 结果 |
| --- | --- | --- |
| 1 | `cargo check --manifest-path src-tauri/Cargo.toml --locked --all-targets` | 退出码 0（在含本任务改动的快照上） |
| 2 | `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib app::views::compare -- --nocapture` | ok：**14 passed / 0 failed / 0 ignored**（本模块全部用例，含端到端与三档宽度矩阵） |
| 3 | `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib` | ok：**930 passed / 0 failed / 7 ignored** |
| 4 | `cargo clippy --manifest-path src-tauri/Cargo.toml --locked --lib --tests` | 退出码 0（含本任务最终改动，全 crate 无 error、`app/views/compare/**` 无 warning） |
| 4b | `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | 首次全量运行退出码 0（当时 `app/views/compare` 还有一条 `assertions_on_constants` 提示，已改为常量处的编译期断言消除）；其后两次复跑分别被 T12（`views/export`）、T13（`views/redis`）未落盘改动挡住，与本任务文件无关 |
| 5 | `git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock` | 退出码 0（依赖未变） |

## 本任务新增的用例

界面层（`src-tauri/src/app/views/compare/mod.rs`，`mod tests`）

- `sides_only_stack_below_the_side_by_side_width`：1360 并排、阈值本身并排、阈值下一像素起竖排、900/640 竖排（正反两侧）。
- `a_lost_compare_fails_with_retry_while_cancel_stays_a_notice`：失败态标题/原因/重试都在文字投影里；取消只有横幅、不含“重试”；清单读取失败换标题；清掉提示后回到初始指导。

界面层（`mod workbench`，真实 GPUI 窗口）

- `sides_reflow_at_1360_900_and_640_in_both_themes_and_densities`：4 组（浅/深 × 紧凑/标准）× 3 档宽度 = 12 次真实渲染，
  断言两张卡片与首行候选连接都在窗口内、1360 顶部同高且左右分列、900/640 同列上下排列；实测几何见
  `widths-themes-densities.md`。
- `narrow_640_keeps_the_guide_its_action_and_the_core_operations_reachable`：640 下初始指导是统一空态、主操作在块内且不被裁切；
  点它触发门禁而不是空白；两侧选择、交换、比较主按钮在 640 下都可点且不出右边界。
- `a_failed_compare_shows_the_error_state_and_retry_clears_it`：失败 → `error-state` + “重试”；点重试后错误态收掉、重发比较照旧走门禁；取消只出横幅、没有重试按钮。

## 既有用例（行为不回退，本轮通过）

- `sqlite_compare_shows_diffs_cancel_and_places_script_without_running_it`：真实 SQLite 夹具 + 真实窗口。
  覆盖比较结构、切选择、比较数据、运行中取消、取消后再次比较、脚本放入编辑器；并两次 `assert_db_unchanged`
  证明结构与数据脚本都没有执行。
- `rendered_controls_pick_database_schema_table_and_compare_key`：库/schema/表/唯一键选择与失效。
- `context_carries_the_object_side_and_explains_what_is_missing`、`reopening_the_context_with_another_baseline_drops_the_old_report`、
  `place_waits_for_confirmation_and_target_change_drops_it`、`swap_filter_select_and_a_late_result_keep_cancel`、
  `schema_still_loading_blocks_the_script`、`different_or_unsupported_engines_do_not_describe_a_script`：全部通过。

## ignored / 被跳过的用例

- 7 条 ignored 属于全仓其它模块（需要真实数据库夹具或平台能力），本轮未启动容器，不计作通过。
- `app::views::compare::live::live_engines_compare_a_made_difference_and_read_the_diagram`
  本轮**没有真正跑比较**：本机没有 PostgreSQL/MySQL/MariaDB/SQL Server 夹具，用例打印
  “跳过结构比较：本机没有数据库夹具”后返回，测试框架计 ok。这一条不算通过证据，只是“夹具不可达时不让整轮变红”。

## 并发与归属（如实记录）

- 本任务全程与 T3（`shell/**`、`model.rs`、`connections/**`、`i18n/mod.rs`）、T10（`views/diagram`）、
  T12/T13（`views/export`、`views/redis`）同批推进，Cargo 目标目录被多方共用。
- 期间出现过三种与本任务无关的瞬时红：`app::shell` 7 条失败（T3 未落盘）、
  `views/diagram` 单测长时间不结束（T10 未落盘）、`views/export`/`views/redis` 编译失败（T12/T13 未落盘）。
  这些都在各自落盘后消失；本任务文件（`app/views/compare/**`）在任何一次编译里都没有产生 error 或 clippy warning。
- 第 3 条 930 passed 的全量结果是在 T10 落盘、T3/T12/T13/T15 尚未落盘的时刻取得的；若后续兄弟任务再改，
  交付 Owner 的最终 `delivery_verification` 才是权威计数。
- 本回执定稿时（04:15 左右）工作区再次编译不过，全部 error 落在 `views/admin`（T15）、`views/redis`（T13）、
  `shell/**` 与 `shell/conn_modal.rs`（T3）上，`app/views/compare/**` 没有任何 error 或 warning。
  因此上面第 2、3、4 条的计数取自 03:56–04:10 之间、本任务代码已定稿的那些运行，不是“现在这条树”的计数。

## 未取得项

- 原生窗口截图（浅/深色 × 紧凑/标准 × 1360/900/640）：本机没有可自动化的桌面会话，见
  `widths-themes-densities.md` 的“证据边界”，由 Delivery Owner 在集成 candidate 上补。
