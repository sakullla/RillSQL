# 测试与命令回执（T9）

全部命令在仓库工作区（HEAD `778b98d` + 本任务改动）上于 2026-11-14 实际执行。

## 1. 正式命令

| 命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked app::views::structure` | 通过：**16 passed / 0 failed**（894 filtered out），含 8 个本任务新增用例 |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked` | 通过：lib **903 passed / 0 failed / 7 ignored**（23.89s）；main 0；rillsql-package 1 passed |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | 通过（exit 0）：34 条 warning，与 T1 记录的既有 34 条同量；本文件只有 1 条且是既有代码（`mod.rs:1951` 的 `unwrap_or_else(Vec::new)`，HEAD 已存在） |
| `git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock` | 通过（exit 0），Cargo 依赖未变 |

按计划，这三条由 Delivery Owner 在集成后的同一 candidate 上重跑；本回执只证明本任务范围内通过。

## 2. 本任务新增用例（`app::views::structure::tests`）

| 用例 | 覆盖 | 证据类型 |
| --- | --- | --- |
| `structure_lists_share_one_row_height_and_fit_the_narrowest_window` | 三张列表的表头高/行高都等于 `Metrics::row_h`（紧凑 26 / 标准 32 / 宽松 40）、内边距统一、每张表最小列宽之和在 640 宽窗口里放得下；反向断言行高不是与密度无关的常量 | 源码级 |
| `empty_lists_have_no_header_and_loading_wins_over_error` | 空列表不画表头；加载优先于错误，错误优先于空态 | 源码级 |
| `production_warning_name_gates_the_confirm_button` | 生产环境未输入对象名时确认按钮禁用；对象名匹配后解除；非危险页签不受约束；确认层标题对象名（已有对象用表名，新建表用输入名） | 源码级 |
| `primary_action_stays_outside_the_scrolled_body_at_every_width` | 1360/900/640 × 900 下"预览 SQL"按钮都在滚动内容之外且在窗口内；行高等于 `Metrics::row_h` | headless 布局级 |
| `danger_dialog_keeps_its_actions_visible_in_a_short_narrow_window` | 640×640 里确认层高 ≤ 0.92×视口、正文与页脚分工（正文滚动）、取消与确认按钮都不越出窗口 | headless 布局级 |
| `row_menu_actions_only_edit_the_draft_and_never_execute` | 字段行菜单项内容（复制名称 / 标记删除）；复制只写剪贴板；标记删除与取消标记只改草稿，不打开预览、不进入可执行状态 | 模型 + 剪贴板 |
| `empty_index_list_replaces_the_table_with_an_empty_state` | 空索引列表渲染后**没有 `structure-table-head`**、出现 `empty-state` 块、没有占位行——"不再以表头加破折号代替空态"的渲染级证据 | headless 布局级 |
| `structure_lists_follow_density_in_both_themes` | 浅色/深色 × 紧凑/标准 四种组合下：行高与表头高都等于 `Metrics::row_h`（26/32），主操作仍在位（主题只换配色，几何只随密度变） | headless 布局级（缺像素） |

## 3. 回归相关的既有用例（本文件内，全部通过）

`fixtures_preview_columns_indexes_keys_views_and_danger_without_a_live_engine`、
`dangerous_ddl_needs_a_second_confirm_and_target_change_drops_the_draft`（第一次武装、第二次执行、取消重置）、
`notes_and_load_errors_follow_a_later_locale_switch`、
`typed_structure_fields_are_the_preview_draft`（各页签的 Action 映射）、
`sqlite_structure_cancel_leaves_the_file_and_confirm_applies_the_preview`（文件级：取消不执行、确认后真改表/索引/删表、未确认被拒）、
`tabs_follow_the_object_and_original_names_let_alter_precheck_proceed`、
`indexes_and_foreign_keys_preview_drops_unique_order_and_actions`、
`production_danger_needs_the_object_name_and_a_failed_note_keeps_the_draft`。

## 4. 相邻边界

- 全量 `cargo test` 同时覆盖 `ui::kit`（三态排版与 token 扫描）与其余视图，901 passed / 0 failed，未见因本页改动导致的回归。
- `i18n::tests::both_locales_translate_every_catalog_key` 通过：新增的 15 个 `structure.*` 键中英都有值。

## 5. 未执行 / 不计作通过

- **原生 `rillsql` 启动与截图**：本环境没有原生窗口与截屏通道，未执行。浅/深色 × 紧凑/标准的像素证据缺，需 Delivery 在集成 candidate 上补。
- **7 个 ignored 用例**：均为既有 fixture/基准用例（与 T1 回执列出的同一批），本轮未运行、环境不要求，不计作通过。
- **MySQL/PostgreSQL/MariaDB 容器**：本任务不涉及连接行为，未启动。
- **ContextMenu 弹层的渲染级用例**：headless 测试窗口里库的 `ContextMenu` 不展开（它用 `hitbox.is_hovered` 判定按下是否属于自己，无真实指针时恒为假），因此右键菜单只验证"菜单项 + 动作语义"，行本身的渲染与行高由布局级用例覆盖。

## 6. 过程中事实（与结论分开记录）

第一次跑仓库内测试时，`src-tauri/src/query/tests.rs` 被并行任务写成以 `[object Object]` 开头的 5894 字节文件（HEAD 为 81476 字节），整 crate 测试目标编译失败：
`error: expected item, found \`[\` --> src/query/tests.rs:1:1`。该文件不在本任务范围内，我没有修改它，已把事实交给 Owner 与对应任务；
为不让 T9 停住，期间用"HEAD 的 src-tauri 树 + 本任务两个文件"在 /tmp 做了临时副本编译与测试（14 passed、全量 879 passed / 0 failed）。
该文件恢复后，上面第 1 节的命令在仓库里重跑通过，回执以仓库结果为准。
