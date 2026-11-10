# GPUI 工作台导航补全与网格体验深化 · T7 验收报告

- 日期：2026-11-09
- 任务：T7（响应式、主题密度、语言与焦点一致性验收），依赖 T3、T6
- 基线：`0761a66`（`fix: 接通外键跳转与完整值上限`）
- 代码状态：变更仍在工作区（未提交），本报告与实现同属一个 T7 交付
- 结论：Rust 测试与 Clippy 全绿；显示矩阵、主题/密度/语言、键盘焦点、最新上下文由 GPUI 测试框架内的真实布局与渲染断言覆盖。原生窗口只做了启动冒烟，**没有截图，也没有原生目视矩阵**（原因见「未验证项」）。

## 1. 实际执行的命令与结果

| 命令 | 结果 |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --locked` | lib 单测 **673 passed / 0 failed / 0 ignored**，62.25s；`rillsql` bin 0 个测试；`rillsql-package` 1 passed；doc-tests 0。整体退出码 0。 |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked` | 退出码 0，无 error。仅 2 条 `dead_code` warning，均在 `src/bin/rillsql-package.rs`（`resolve_out_dir`、`strip_verbatim_prefix`），该文件本任务未改动，属既有告警。 |
| `cargo build --manifest-path src-tauri/Cargo.toml --bin rillsql --locked` | 成功，`Finished dev profile in 1m 45s`，产物 `rillsql.exe`。 |
| 原生启动冒烟（见 3） | 进程启动并存活、窗口标题为 `RillSQL`。 |

T7 新增/扩展的测试（均在上述 673 中通过）：

- `app::shell::tests::display_matrix_keeps_primary_controls_visible_and_unoverlapped`（新增，1360×900 / 900×900 / 640×900）
- `app::shell::tests::locale_theme_and_density_refresh_existing_inputs_tree_and_palette`（新增，900×900）
- `app::shell::tests::background_reads_keep_navigation_usable_and_latest_context_wins`（新增）
- `app::views::grid::pane_tests::cancelled_edit_and_closed_viewer_keep_selection_scroll_staging_and_viewer`（扩展：Esc 关闭查看面板后断言焦点回到网格）
- 既有回归仍在跑：`copy_fits_every_width_density_theme_and_locale`、`density_theme_and_language_reach_the_rendered_chrome`、`palette_keyboard_reaches_compare_exchange_and_help_and_restores_focus`、`keyboard_navigation_expands_moves_and_collapses_without_running_sql`、`keyboard_tab_cycling_reaches_every_work_item`、`mouse_and_keyboard_produce_the_same_rectangle_and_copies`、`losing_focus_ends_the_drag_without_moving_the_selection`、`keyboard_editing_uses_a_real_text_input_and_commits_or_cancels`。

## 2. 显示 / 主题 / 密度 / 语言矩阵

「已验证」指在 GPUI 测试窗口里真实布局（`window.render_frame` 后读取 `bounds()` / `theme().is_dark()` / `visible()`）并断言；「未验证」指只在原生窗口里人工可判断、本轮没有取到证据。

### 2.1 视口（宽 × 高 = 900）

| 视口 | 顶栏/活动栏/工作区/状态栏不重叠 | 主操作、关闭、返回可达 | 侧栏不挤压主任务 | 状态 |
| --- | --- | --- | --- | --- |
| 1360×900 | 已验证（四段只相接、各自在视口内） | 已验证（`top-new-query`、`status-locale`、`tab-close-*`、`compare-back`、`help-close`） | 已验证（docked 侧栏，工作区 ≥ 160px） | 离屏已验证 |
| 900×900 | 已验证（同上） | 已验证（同上） | 已验证（侧栏在 flex 内、活动栏固定 52px） | 离屏已验证 |
| 640×900 | 已验证（同上） | 已验证（同上；浮层/命令面板完整落在视口内） | 已验证（`sidebar_overlay`，覆盖而非挤压，工作区 ≥ 160px） | 离屏已验证 |

同一断言还覆盖 `density` 三档（`RAIL_WIDTH` 与 `content_width` 之和必须正好等于视口宽，`layout::tests::rail_width_is_reserved_once_so_the_workspace_is_not_squeezed`），以及顶栏不换行（`toolbar_height` 恒为单行高度）。

### 2.2 主题 / 密度 / 语言

| 维度 | 覆盖 | 证据 |
| --- | --- | --- |
| 浅色 / 深色 | Light、Dark 都切到实际绘制主题（`cx.theme().is_dark()` 翻转） | `locale_theme_and_density_refresh_existing_inputs_tree_and_palette`、`density_theme_and_language_reach_the_rendered_chrome`、`copy_fits_every_width_density_theme_and_locale` |
| Compact / Standard（+ Relaxed） | 切换后**已存在**的对象树行高、根 padding 立即变化 | 同上（compact < relaxed 行高、`painted().pad == density.metrics().pad`） |
| 中文 / English | 已展开树标签、已创建输入框占位、已打开命令面板条目即时换文案，英文无残留中文 | `locale_theme_and_density_refresh_existing_inputs_tree_and_palette`、`english_copy_has_no_chinese_and_query_titles_follow_locale` |
| 长内容 | 文案溢出计数为 0（1360/1024/900/760/640 × 侧栏开合 × 明暗 × 中英 × 三档密度 × 四个页面） | `copy_fits_every_width_density_theme_and_locale` |
| 状态不只靠颜色 | 树行同时给出 `aria_description` 与状态圆点 tooltip（同一句状态文案），状态圆点带 `aria_label` | 本次 `chrome.rs` 改动 + 无障碍节点断言（`access_nodes` 非空 id/role/state） |

### 2.3 键盘可达与焦点归还

| 路径 | 证据 |
| --- | --- |
| 仅键盘完成对象查找 / 展开移动 / 补全 / 筛选 | `keyboard_navigation_expands_moves_and_collapses_without_running_sql`、`keyboard_tab_cycling_reaches_every_work_item`、补全相关 query 测试 |
| 命令面板 → 对比/交换/帮助，关闭后焦点回触发区 | `palette_keyboard_reaches_compare_exchange_and_help_and_restores_focus` |
| 菜单项执行后焦点回原触发区 | 本次 `chrome.rs` `restore_if_closed`/`restore_focus`，由上述回归 + `six_menus_keyboard_and_close_guard_stay_on_one_command_layer` 覆盖 |
| 网格查看面板 Esc 关闭后焦点回网格 | `cancelled_edit_and_closed_viewer_keep_selection_scroll_staging_and_viewer`（新增断言 `is_focused(window)`） |
| 指针取消/失焦不持续拖选 | `losing_focus_ends_the_drag_without_moving_the_selection` |

### 2.4 最新上下文

| 场景 | 证据 |
| --- | --- |
| 后台读取期间仍可切页面、改筛选、切工作项 | `background_reads_keep_navigation_usable_and_latest_context_wins` |
| 快速切换只写回最新一次读取（旧代次被判定 `Stale`） | 同上（`TaskBoard::supersede` + `accept` 的 `Stale`/`Current`） |

## 3. 原生启动（真实执行，仅冒烟）

- 启动：`C:\Users\12976\.cargo\target\debug\rillsql.exe`（debug 构建，非 release 包）
- 应用自身日志首行（stdout 重定向文件）：`gpui-window: starting kit=0.9.0 pre=0.3.11 os=windows arch=x86_64`
- 进程：`tasklist /v` 显示 `rillsql.exe` PID 18164，会话 `Console`（Session 1），内存约 95 MB，状态 `Running`，CPU 累计 `0:00:03`，**窗口标题列 = `RillSQL`**
- 存活观察：启动后 ≥ 20 秒仍在运行，随后用 `taskkill /F /IM rillsql.exe` 结束；被杀进程因此报告退出码 1，其 stdout/stderr 捕获文件除上述一行启动日志外全空，没有 panic 或错误文本
- 原生交互（点击、切主题、缩放到 640×900 等）：**未执行**

## 4. 未验证项与限制（如实记录）

1. **没有截图。** 本环境没有屏幕捕获工具，PowerShell 被策略拦截（`Invoking PowerShell from Bash bypasses PowerShell security checks`），因此无法对窗口取像素，也没有保存任何 `docs/verification/*.png`。原 T7 诉求里的「保存实际 GPUI 截图」未达成，请勿把本报告当作视觉证据。
2. **原生窗口未做目视矩阵。** 1360×900/900×900/640×900 × 明暗 × Compact/Standard × 中英 的组合只有测试窗口内的离屏布局与主题断言，没有在真实窗口里逐个切换确认。真实 DPI 缩放、字体渲染、IME 候选框位置、原生菜单/工具提示层级均未实测。
3. **单次运行不构成因果闭合。** 673 个测试一次全绿，只说明本次工作区状态下通过；任何一项的根因修复都未经过独立复现—修复—再验证闭环。
4. 原生启动只到「窗口出现且进程稳定」为止，未验证连接数据库、真实查询与网格交互；这些在测试里走的是 `services_at` 的临时 SQLite，不等于真实引擎行为。
5. Clippy 通过但保留 2 条既有 `dead_code` warning（未改动文件），未顺手清理以免扩大范围。
6. **本文件当前被 `.gitignore` 忽略。** 规则是 `docs/verification/*` 之外只放行 `docs/verification/gpui-foundation/**`（该目录现在也不存在），所以本报告不会进入提交，需 Owner 决定是调整忽略规则还是改放其它目录；本任务没有擅自改动 `.gitignore`。

## 5. 修改路径

- `src-tauri/src/app/shell/chrome.rs`：活动栏宽度取自 `layout::RAIL_WIDTH`；浮层/菜单/帮助关闭后归还真实键盘焦点；树行补 `aria_description`、状态点补 `aria_label`；关键节点加 `test_support`。
- `src-tauri/src/app/shell/layout.rs`：新增 `RAIL_WIDTH = 52`；顶栏不再换行（`toolbar_wrap` 恒 false、高度恒为单行）；新增活动栏/内容宽一致性测试。
- `src-tauri/src/app/shell/model.rs`：`copy_overflow` 改用 `painted().content_width`，窄窗口裁切会计入溢出。
- `src-tauri/src/app/shell/view.rs`：`focus` 提升为 `pub(super)`，供焦点归还使用。
- `src-tauri/src/app/shell/tests.rs`：新增显示矩阵、语言/主题/密度即时刷新、最新上下文三个测试。
- `src-tauri/src/app/views/grid/mod.rs`：新增 `request_focus()`；Esc 关闭查看面板/预览后置 `refocus`，并补焦点归还断言。
- `docs/verification/2026-11-09-GPUI-workbench-navigation-grid-experience.md`：本报告。

## 6. 复现

```bash
cargo test  --manifest-path src-tauri/Cargo.toml --locked
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked
cargo build --manifest-path src-tauri/Cargo.toml --bin rillsql --locked
```
