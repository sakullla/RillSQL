# ER 关系图页（T10）验证目录

- 日期：2026-11-14
- 任务：T10「统一 ER 图页面的卡片交互、键盘焦点、三态和窄宽操作，不重做图功能」
- 覆盖需求：R1（呈现与交互与设计基准一致）、R3（四态、键盘等价、三态各带下一步）、R4（1360/900/640 布局与主操作可达）
- 覆盖规范：S2 第 6/7 条（全部控件四态、三档宽度无横向滚动）、S5.3（四态与光标）、S5.6（可 Tab 到达、图标按钮有 aria-label）、S7.3（断点）、S8.5（空/错误态说明发生了什么与下一步）、S12.3（`kit::interactive` 四态落地）、S12.4（三态入口）
- 本次提交：`src-tauri/src/app/views/diagram/mod.rs`（状态判定、卡片四态与键盘等价、窄宽工具条、各状态下一步）+ 本目录文档。界面用的三条 `diagram.emptyDetail` / `diagram.searchNoMatch` / `diagram.searchClear` 文案随提交 `3c21298` 进入 `main`，本任务**不重复认领** `src-tauri/src/i18n/mod.rs`
- 未改：`src-tauri/src/diagram/`（读取与连线算法）、`src-tauri/src/app/shell/`（外壳与页面切换）、Cargo 依赖

## 文件

| 文件 | 内容 |
| --- | --- |
| `diagram-audit.md` | 逐条偏差审计（源码入口—偏差—修复结果—验证材料），含只读边界与“不重做图功能”的边界说明 |
| `widths-1360-900-640.md` | 1360/900/640 × 900 的页头换行、卡片、缩放、键盘与四态证据 |
| `theme-density-matrix.md` | 浅/深色 × 紧凑/标准 下的卡片四态与三态取值矩阵，以及截图的取得情况 |
| `test-receipts.md` | 本轮实际运行的命令、结果、用例清单与未取得项 |

## 证据类型（请勿混读）

- **源码级**：`PaneState` 判定、`card_key`/`pane_key` 的键盘语义、四态取值来自 `kit::interaction_style`，由 `cargo test` 纯断言覆盖（`card_keyboard_equivalents_match_the_mouse_gestures`、`pane_keys_zoom_and_dismiss_without_touching_the_diagram`、`four_states_carry_the_matching_next_step`）。
- **headless 布局级**：GPUI 测试窗口里真实渲染出来的元素与可访问名称（`diagram-card:*`、`empty-state`、`error-state`、`state-action`、`diagram-tools`），按 1360/900/640 × 浅/深色 × 紧凑/标准 逐格断言（`diagram_four_states_and_keyboard_hold_at_1360/900/640`）。
- **原生行为级**：SQLite 夹具上的真实读取（`sqlite_diagram_shows_cards_wires_hover_and_does_not_write`）：卡片、连线、悬停关联、双击打开请求，以及读取前后 `sqlite_master` 与行数不变。
- **未取得：原生像素截图**。本轮没有原生窗口与截屏通道的使用授权，未生成 PNG。按计划由交付阶段在最终集成 candidate 上采集，不用历史截图替代；缺口同时列在 `theme-density-matrix.md` 与 `test-receipts.md` 的「未取得项」。

## 结论摘要

- 可点表卡片现在有四态：hover 底色、按下底色、键盘 focus-visible 2px 强调色焦点环（只在键盘导航时出现）、禁用语义（卡片本身不提供禁用路径，四态取值仍来自 `kit::interaction_style`，随底座变化）。
- 鼠标手势都有键盘等价：Enter = 双击（打开表数据），Shift+Enter = 看表结构，菜单键 / Shift+F10 = 右键菜单，Esc = 收起菜单与悬停；鼠标侧点菜单条以外的页面任意位置（画布、页头、页脚）也会收起菜单，与启动器筛选浮层、导出抽屉一样靠“点别处”关闭；缩放 + / - / 0 与工具栏按钮等价，搜索框聚焦时页面快捷键不抢键。
- 四种状态各有下一步：读取中＝局部加载；失败＝错误块 + 重试；空图＝说明为什么空 + 重新读取；无目标＝无数据空态 + `Ctrl+B` 打开资源管理器选表。无目标不再画成红色错误块。
- 页头在 640 宽换成第二行工具条（搜索框占满剩余宽度），1360/900 保持单行；三档宽度下卡片、缩放、打开对象、四态都可达，页面本身不出现横向滚动。
- 只读边界不变：整个过程不发送任何 DDL/DML，读取前后 `sqlite_master` 与 `orders` 行数一致。
- 页头「返回」按钮的消费点在 `src-tauri/src/app/shell/view.rs`：渲染路径上的 `consume_diagram_back`（约 2038 行）取走 `DiagramPane::take_back()` 后调用 `model.return_to_work()`，回到上一个工作项。
