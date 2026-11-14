# 历史页与设置页（T8）验证目录

- 日期：2026-11-14
- 任务：T8「统一历史与设置页的密集布局、键盘、三态、主题密度和持久化反馈」
- 覆盖需求：R1（逐屏审计）、R3（四态与交互一致）、R4（三态、双主题、密度、三档宽度）
- 覆盖规范：S5.2（列表由容器持有焦点、当前行用选中表达）、S8.2（历史页）、S8.3（设置页分区导航）、
  S8.5（空态与错误态，"无行"与"无命中"文案必须不同）、S9（主题/密度/响应式）、S12（密度派生尺寸）
- 基线：A1 为 HEAD `0ff974d` + 任务改动；A2 修复（2026-11-14）为 HEAD `441ef6f` + 本次改动。
  Cargo 依赖未变（`git diff --exit-code -- src-tauri/Cargo.toml src-tauri/Cargo.lock` 通过）
- 改动范围（A1 + A2 合计）：`src-tauri/src/app/views/history/mod.rs`、`src-tauri/src/app/views/settings/mod.rs`、
  `src-tauri/src/app/shell/chrome.rs`、`src-tauri/src/app/shell/model.rs`、`src-tauri/src/app/shell/view.rs`、
  `src-tauri/src/app/shell/jobs.rs`（`saved_id` 改用钉住的删除目标，A2）、`src-tauri/src/app/shell/tests.rs`、
  `src-tauri/src/i18n/mod.rs`（仅追加 14 个文案键，A2 未新增键）

## 文件

| 文件 | 内容 |
| --- | --- |
| `history-settings-audit.md` | 两页逐条审计：源码入口、本次之前的偏差、改动、验收对应与证据引用 |
| `theme-density-and-widths.md` | 实际 headless 几何数值（行高、卡片高、导航宽）与三档宽度行为，双主题双密度取值来源 |
| `test-receipts.md` | 实际运行的命令、结果、新增用例清单与未执行项；第 5 节是 A2 修复的回执与因果验证 |

## 证据类型（请勿混读）

- **源码级**：`cargo test` 里对模型/分类函数的断言（状态判定顺序、命令 id、文案键、写盘状态机）。
- **headless 布局级**：GPUI 测试窗口（1360/900/640 × 800）里读出的元素边界，证明行高随密度变化、
  页面与卡片不越出窗口、窄宽把设置导航折到表单上方、状态块主操作 38px 缩进。
- **未取得的证据：原生截图与原生窗口操作**。本次执行环境没有可用的原生应用窗口，
  因此**没有任何浅色/深色 × 紧凑/标准截图**，也没有原生启动、Tab 走查或 IME 证据。
  本目录不复制历史截图顶替，也不用静态推断冒充。原生截图入口与限制见 `theme-density-and-widths.md` 第 4 节。
