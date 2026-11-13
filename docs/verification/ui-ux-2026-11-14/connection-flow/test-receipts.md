# T3 测试回执

环境：macOS（本机），Rust 1.90+，`--locked`（Cargo.toml / Cargo.lock 未改动：`git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock` 为空）。
所有命令都在仓库根 `/Users/yanggeng/github/RillSQL` 下执行。

## 1. 全量 Rust 测试

```
cargo test --manifest-path src-tauri/Cargo.toml --locked
```

结果：**exit 0**

| target | 通过 | 失败 | ignored |
| --- | --- | --- | --- |
| `rillsql` lib | 930 | 0 | 7 |
| `src/main.rs` | 0 | 0 | 0 |
| `rillsql-package` | 1 | 0 | 0 |

7 条 ignored 全部是本轮之前就存在的 fixture 用例（MySQL 回环 33067、真实 SMTP/SSH 等），**不计入通过**。

## 2. Clippy

```
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked
```

结果：**exit 0，0 error，33 warning**。warning 全部落在本任务未改动的模块
（`views/grid`、`views/diagram`、`ui/editor_proto` 等既有的 `too_many_arguments` / `private_interfaces`）。
本任务新增代码只沿用文件里既有的 `#[allow(clippy::too_many_arguments)]` 处理方式（`launcher_action`、`filter_button`、
`modal_action`、`path_row`），并修掉了自己引入的 `collapsible_match` / 未使用 import / 未使用变量。

## 3. 设计 token 门禁

```
ui::kit::token_tests::style_literals_follow_design_tokens ... ok
ui::kit::token_tests::style_scan_rejects_reverted_literals ... ok
```

新写的启动台与模态没有引入裸字号/圆角字面量：字号走 `TypeScale`、圆角走 `Radius`、高度走 `Metrics::control_h/row_h/
menu_row_h`、颜色走 `Palette`。

## 4. 本任务新增/更新的用例（全部 ok）

| 用例 | 证的行为 |
| --- | --- |
| `app::shell::conn_modal::tests::tab_order_stays_inside_the_modal_and_wraps` | Tab/Shift+Tab 在模态停靠点里成环，末项回首个字段 |
| `app::shell::conn_modal::tests::stops_follow_the_shape_of_the_form` | 停靠点随引擎/TLS/SSH 形状变化（SQLite 无 host、密码认证无私钥） |
| `app::shell::conn_modal::tests::a_stale_test_result_never_clears_the_newer_indicator` | 旧代号不能收走“正在测试”（P3 #1，模态侧） |
| `app::shell::conn_modal::tests::closing_returns_focus_once_and_clears_pending_state` | 关闭只还焦一次，清掉待续接/关闭态 |
| `app::shell::tests::every_entry_opens_the_same_modal_and_cancel_leaves_no_draft` | 四个入口同一模态、取消不保存不留草稿、还焦触发控件 |
| `app::shell::tests::modal_tab_order_wraps_inside_and_covers_fields_and_footer` | 模型侧 Tab 序列覆盖字段+页脚并成环 |
| `app::shell::tests::plain_save_closes_the_modal_without_connecting` | 普通保存：关模态、0 个连接任务、最近使用为空 |
| `app::shell::tests::save_and_connect_queues_one_open_and_keeps_the_action_for_retry` | 保存并连接：只排一次 OpenConnection；失败保留动作可重试 |
| `app::shell::tests::a_failed_test_keeps_the_draft_and_marks_the_field` | 测试失败：草稿/机密保留、`issues[0].field == file_path`、模态不关 |
| `app::shell::tests::an_older_test_result_cannot_clear_the_newer_running_indicator` | 真实任务链下旧结果不覆盖新一次指示（P3 #1，shell 侧） |
| `app::shell::tests::on_demand_connect_queues_the_action_and_replays_it_once` | 按需连接：排队一次、在途复用、连上后按顺序各重放一次、已连接直接执行 |
| `app::shell::tests::filter_popover_flips_inside_the_viewport` | 筛选浮层右/下边缘碰撞翻转到视口内 |
| `app::views::connections::tests::a_late_result_never_clears_the_newer_running_indicator` | 页面级代号门禁（P3 #1） |
| `app::views::connections::tests::an_edited_draft_drops_its_own_result_and_only_stops_the_indicator` | 同代改字段：只收指示，不写消息/字段错误 |
| `app::views::connections::tests::failure_maps_to_the_field_the_user_should_fix`（更新） | Access denied / WRONGPASS 落到 `credentials`（P3 #2） |
| `app::shell::tests::connections_empty_state_wraps_inside_its_panel_at_every_width`（更新） | 启动台空态在 1360/900/640 里都落在页面内、最宽 360 |
| `app::shell::tests::density_theme_and_language_reach_the_rendered_chrome`（更新） | 640 宽下模态的取消/测试/保存/保存并连接都在视口内；dark/light × compact/relaxed 真实控件高度 |
| `app::shell::tests::sidebar_connection_menu_opens_the_visible_editor` | 右键行“编辑”打开就是那个模态（`conn-form` 可见，id 正确） |
| `app::shell::tests::sidebar_copy_and_test_remain_usable_while_catalog_is_loading`（更新） | 先关模态，右键仍可复制配置与测试；测试结果回到 `test.ok` |
| `app::shell::tests::headless_workbench_renders_on_windows_widths` | 1360/900/640 三档真实绘制（既有，未回退） |

## 5. 三档宽度 × 双主题双密度的源码级证据矩阵

| 维度 | 用例 | 断言的几何/状态 |
| --- | --- | --- |
| 1360 | `density_theme_and_language_reach_the_rendered_chrome`、`headless_workbench_renders_on_windows_widths`、`connections_empty_state_…` | 启动台标题行与入口不越右缘；模态按钮在视口内；空态说明 ≤360 且不越窗 |
| 900 | 同上、`copy_fits_every_width_density_theme_and_locale` | 同左；每页首要操作可见、可见宽度按密度切换 |
| 640 | 同上 + `long_copy_…` | 模态面板按窗口收窄（`panel_w = view_w - 40`），取消/测试/保存/保存并连接仍可见可点；长名称行不越界 |
| 浅色 / 深色 | `density_theme_…`（`theme-dark` → `theme-light` 断言 `cx.theme().is_dark()`） | 主题切换落到实际绘制主题，模态与启动台用同一 `Palette` |
| 紧凑 / 标准（含宽松） | `density_theme_…`（compact 26 → relaxed 32）、`copy_fits…` | 控件高度跟着 `Metrics` 变，行高/菜单行高同源 |

## 6. 未取得的证据（不冒充）

- **原生窗口与截图 PNG**：本环境没有可用截屏通道，`docs/verification/ui-ux-2026-11-14/connection-flow/` 下没有
  1360/900/640 或双主题双密度的图片。Delivery Owner 需在集成 candidate 上补。
- **真实输入法与实体 Tab 键**：headless 窗口能断言焦点落点（`FocusHandle::is_focused`）与停靠序列，
  不能证明原生输入法组合键与系统 Tab 焦点环的实际像素表现。
- **数据库 fixture**：本任务的连接端到端用本地 SQLite 文件；PostgreSQL/MySQL/MariaDB 的真实连接未在本任务内跑
  （ignored 的 fixture 用例不计通过）。
