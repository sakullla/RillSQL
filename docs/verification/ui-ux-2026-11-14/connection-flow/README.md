# 连接模态与启动台（T3）验证目录

- 日期：2026-11-14
- 任务：T3「将新建/编辑连接重做为全局模态，并把连接管理页与 welcome 收敛为带完整入口的启动台和按需连接流程」
- 覆盖需求：R1（连接页/欢迎区版式与入口）、R2（截图缺陷：连接表单与错误呈现）、R3（键盘与浮层）、R4（四态与密度）、R6（连接端到端流程）
- 覆盖规范：S8.1（连接页）、S8.5（空态与错误态）、S8.6（欢迎区）、S5.6（浮层焦点陷阱）、S12.3（四态落地）
- 改动范围：`src-tauri/src/app/shell/{conn_modal.rs,model.rs,chrome.rs,view.rs,jobs.rs,menus.rs,tests.rs}`、
  `src-tauri/src/app/model.rs`、`src-tauri/src/app/views/connections/mod.rs`、`src-tauri/src/app/views/welcome/mod.rs`、
  `src-tauri/src/i18n/mod.rs`（仅追加启动台小节文案键）

## 文件

| 文件 | 内容 |
| --- | --- |
| `behavior-map.md` | T3 六条 outcomes 逐条对应实现入口、行为取舍、回归用例，以及 T2 review 两条 P3 的关闭方式 |
| `test-receipts.md` | 本轮实际运行的命令、通过/失败计数、环境与限制，含三档宽度/双主题双密度的源码级证据清单 |

## 证据类型（请勿混读）

- **源码级 + Rust/GPUI 测试**：本目录结论来自 `cargo test`、`cargo clippy` 与本轮源码。连接测试使用真实 SQLite 文件与
  内存凭据库；渲染证据来自 `gpui_kit::test` 的 headless 窗口（真实布局几何，不是静态推断）。
- **未取得：原生窗口与截图 PNG**。本任务所在环境没有可截屏的原生窗口通道，因此
  `connection-flow` 下没有 1360/900/640 三档与浅/深色×紧凑/标准 的截图。按计划由 Delivery Owner 在最终集成
  candidate 上取得；本目录不用历史截图替代，也不把 headless 渲染说成原生截图。
- **1360/900/640 端到端场景**：以 headless 窗口的真实绘制几何断言代替（见 `test-receipts.md` 的宽度矩阵）。
  这能证明“按钮在视口内、说明不越界、空态只有一份”，不能证明原生窗口的字体栅格与真实输入法行为。
- **双主题双密度**：`density_theme_and_language_reach_the_rendered_chrome` 覆盖 dark/light 与 compact/relaxed 下
  模态与启动台按钮的真实高度与边界；没有对应 PNG。
