# T5 统一逐步回执 —— 集成记录

需求：`docs/requirements/2026-11-21-mysql-proprietary-support.md` 的 R8（执行结果可观察、部分失败与原始错误可读）、R10（危险操作与失败语义不静默）。
Task：`03-execution-plan.md#tasks/T5`。本文件只记录实际执行过的命令与结果，没有跑过的检查不写成通过。

## 改了什么

- 新增共用组件 `src-tauri/src/app/views/structure/receipt.rs`（`pub(crate) mod receipt`）：
  - `ReceiptOutcome::of(&Receipt)` → `Blocked / NothingRun / Done / Partial`；`is_success()` 只有 `Done`；
    `note_key()`、`rereads_target()`、`keeps_draft()` 都由这一个结局推出来。
  - `rows(&Receipt)` 给出每一步的 label、SQL、状态、原始 message；`problems(&Receipt)` 给出被拦下原因
    或没成功的每一步原始 message（没有 message 时退回 SQL；没有任何语句时给“没有执行任何语句”）。
  - `panel(id_prefix, receipt, p, locale)` 渲染同一套逐步回执，id 前缀由调用方给
    （`{prefix}-receipt`、`{prefix}-step-{label}`、`…-sql`、`…-message`）。
- 结构页（`app/views/structure/mod.rs`）：`StructureSession` 保存最近一次回执；新增
  `apply_receipt` 统一算结局、写提示、只在全部成功时收起预览；目标不一致的旧回执只提示
  `structure.execute.stale`，不覆盖当前目标。部分成功调 `reread_after_partial()` 读回服务器结构，
  但不重置草稿起稿状态，用户填的字段/分区/视图定义都留着。回执在正文与预览卡片里都显示。
- 管理页（`app/views/admin/mod.rs`）：`AdminSession` 同样保存回执并有 `apply_receipt`；部分成功时
  `refresh(cx)` 重新读清单/对象，表单输入不动；目标不一致只提示 `admin.note.executeDropped`。
  对象分节失败仍按 `AdminInventory.faults` 逐节隔离（原有 `push_fault` 路径），一节失败不影响别的节。
- 对象编辑器（`app/views/object_editor/`）：`receipt_panel` 改为调用同一组件（前缀保持
  `object-editor`）；`receipt_all_ok`、`step_status_text`、`keeps_draft` 全部委派到共享判定；
  `after_execute` 的三种结局（Done/Failed/Partial）与草稿、读回策略保持 T3 的行为。
- 危险操作层（`app/shell/danger.rs`）：`receipt_problems` 委派共享 `problems`；`DangerPrompt` 保存
  最近一次 `Receipt` 并提供 `receipt()`，`apply_receipt` 仍以“全部 Ok”为通过。
- 危险操作确认层渲染（`app/shell/chrome.rs` 的 `danger_confirm_layer`，danger-error banner 之后）：
  用 `DangerPrompt::receipt_rows()` / `receipt_blocked()` 取行，再交给同一份渲染
  `structure::receipt::panel_rows("danger", blocked, rows, &p, locale)`（`panel` 内部也走它）。元素 id 为
  `danger-receipt`、`danger-receipt-blocked`、`danger-step-{label}`、`…-sql`、`…-message`。原有的
  plan.sql / risks / blocked / error 展示全部保留，只是额外逐条列出最近一次回执；成不成功仍由共享
  `ReceiptOutcome` 判定（只有全部 Ok 才算）。该文件不在 T5 原计划 scope 里（原 scope 只列了
  `app/shell/danger.rs` 与 `app/shell/tests.rs`），属 T5 outcome 3“逐步状态可观察”所必需的漏列项，
  已由 Owner 授权后补上，且只有这一处渲染改动。
- `ddl/tests.rs:1841` 的 `err().expect(...)` 改为 `expect_err(...)`（clippy `err_expect`）。
- `ddl/english.rs` 的中文字面量扫描表加入 `app/views/structure/receipt.rs`，并把“必须扫到中文字面量”
  的代表文件从 `app/shell/danger.rs` 换成该文件——文案已搬过去，守卫范围跟着走（此文件不在 T5 声明的
  scope 内，属必要连带改动，已在交给 Owner 的结果里标注）。

## 实际执行的命令与结果

| 命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked` | 通过：lib 1662 passed / 0 failed / 14 ignored；`rillsql-package` 1 passed；doc-tests 0 |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | 通过：0 warning、0 error |

两条命令都在本机 Windows 上对当前工作树执行，最后一次全量测试在修掉 clippy 报的 `needless_borrow`
之后重跑，结果即上表。

## 覆盖到的场景（Rust 测试）

新增/更新的测试：

- `app::views::structure::tests::only_all_ok_receipt_closes_the_preview_and_other_outcomes_keep_it`
  - 被拦下：不收起预览、提示就是原始原因、没有逐步行；
  - 没有被拦下但没有语句（NothingRun）：同样不算完成；
  - 多步部分失败（Ok + Failed）：预览留着、提示“部分完成”、回执行给出两步的 label/SQL/状态/原始 message；
  - 全部 Ok：收起预览并显示统一的“已按预览执行。”。
- `app::views::structure::tests::a_receipt_for_another_target_only_reports_stale`：目标不同的旧回执
  只提示 `structure.execute.stale`，不写入回执、不关闭当前预览、不覆盖当前目标。
- `app::views::structure::tests::outcome_rules_for_rereading_and_draft_keeping`：Blocked / NothingRun /
  Done / Partial（Failed、Skipped、Unknown、RolledBack）六种输入的结局、`rereads_target`、`keeps_draft`、
  `problems` 取值；Failed 且没有 message 时退回 SQL。
- `app::views::admin::tests::admin_receipts_use_the_shared_outcome_and_keep_the_form`：管理页同一套结局，
  部分成功不收起预览且保留表单，成功才收起，目标不同的回执只提示过期。
- `app::views::admin::tests::a_fault_in_one_object_section_does_not_block_the_others`：事件一节读取失败
  只产生这一节的 `SectionFault`，其它节（分区/序列/附加库/PRAGMA）不被连带标记失败，另一类对象的管理
  计划照旧能生成、不被阻断。
- `app::shell::danger::tests::multi_step_receipts_keep_every_row_and_only_report_the_failed_ones`：
  危险操作层多步回执只有失败步骤进错误、全部 Ok 才算通过、回执整份留着供逐步显示，重新预览会丢掉旧回执。
- `app::shell::tests::danger_layer_renders_each_receipt_step`（`#[gpui_kit::test]`，真实渲染）：在
  `open_workbench_sized` 的窗口里把带部分失败回执的 `DangerPrompt` 放进 model，`render_frame` 后断言
  `danger-receipt`、`danger-step-drop`（成功步）、`danger-step-verify`（回滚步）与 `danger-error` 都可见——
  这是 chrome 渲染入口的实际覆盖，不是视觉像素验收。
- `app::shell::tests::danger_layer_keeps_every_step_of_a_partial_receipt`：走生产路径
  `WorkbenchModel::danger_finished` → `DangerPrompt::apply_receipt`，确认层在多步部分失败后仍开着、
  错误只有失败步骤的原文、整份回执（两步、状态、message）留给 `danger_confirm_layer` 逐条渲染。
- 对象编辑器原有三种结局测试（`receipt_tests::*`）保持通过，确认 T3 行为未被改动。
- `ddl::english::tests::every_chinese_literal_on_ddl_paths_has_an_english_form` 通过：新组件里的中文文案
  都有英文形式。

## 没有执行的部分

- 没有启动 GPUI 桌面应用做人工界面核对（`cargo run --bin rillsql`）：颜色、间距与遮挡关系属于人工观察项，
  本次只由 Rust 测试覆盖状态与文案，未宣称界面视觉已验收。
- 没有连接真实 MySQL/MariaDB/PostgreSQL 服务器跑逐步回执：T5 的判定与展示是纯模型 + 组件逻辑，真实执行链
  的多步成功/失败由 T2/T3/T4 的实库脚本覆盖，本次未重复执行那些脚本。
- 危险操作确认层的逐步回执有渲染层断言（`danger-receipt` 等元素可见），但没有做人工视觉核对：
  配色、间距、长 SQL 滚动这些仍属人工观察项。
