# 对象比较页审计（T11）

依据：`specs/2026-10-17-desktop-ui-ux-redesign-design.md`（S1–S12）、需求 `2026-11-14-desktop-ui-ux-unification.md`
的 R1（统一底座三态）、R3（一致交互）、R4（三态语义）、R6（三档宽度不出页面横向滚动）。
底座入口一律取自 `src-tauri/src/ui/kit.rs`（T1 落地），页面不再自带一套三态。

## 改动前偏差（按页面从上到下）

| # | 位置 | 改动前 | 为什么算偏差 |
| --- | --- | --- | --- |
| A1 | 初始指导（无报告、无错误） | `kit::empty_state(..., detail="")` | 说明是空字符串，也没有主操作：用户看到标题后无处可点（S8.5 要求状态带下一步） |
| A2 | 运行中（比较结构/数据） | 动作行里裸 `Spinner::new().xsmall()` + 一句「正在对比…」，详情区全空 | 裸转圈不是底座语义；详情区在整块读取期间空着，没有骨架也没有可读的进度 |
| A3 | schema 读取中（PostgreSQL/SQL Server 侧卡片） | 手拼 `Spinner + 文字`，未贴左、未按 `strip_h` 单行高 | 同 A2：页面自己发明局部加载，与其它视图不同高不同位 |
| A4 | 连接清单读取中 | `choices_loading` 只有字段、没有渲染；列表先显示「选择连接」 | 把“还在读”画成“这里是空的”，用户会以为没有已打开连接 |
| A5 | 比较错误 | 门禁（上一次还在读）、取消、过期、任务丢失、清单读取失败共用一条 danger 横幅，没有动作 | 语义该分：门禁/取消/过期是提示，任务丢失与清单读取失败是失败，要能重试；一条横幅既不区分也点不动 |
| A6 | 窄窗口（900 / 640） | 两侧恒为 `flex_1` 并排，640 时每侧内宽只剩约 150，连接/库/schema 三个选择块互相挤压 | 违背 R6：窄宽要重排成可用布局，而不是把两列一起压扁 |

## 逐条处置

| # | 处置 |
| --- | --- |
| A1 | 改用 `kit::empty_state_with_action`：说明取 `compare.dialog.subtitle`（对比产出什么、脚本生成时不执行），主操作「对比结构」放进状态块（38px 内缩，与标题左对齐，不再用 `.child()` 追加按钮）。块内按钮 id 为 `compare-guide-start`，命令仍是 `compare-structure`，与工具条按钮不重名。 |
| A2 | 动作行改用 `kit::inline_loading`（`strip_h` 单行高、贴左）；「正在比较且还没有任何报告」时详情区放一张卡片：一条局部加载 + `kit::skeleton_state(5)` 骨架。已有旧报告时保持旧报告可见，等结果替换，不闪空白。 |
| A3 | 侧卡片 schema 读取改用 `kit::inline_loading`；`Manual` 分支（读取失败需手填）不变。 |
| A4 | 清单读取中（`choices_loading`）渲染 `kit::skeleton_state(3)`；读完仍为空才显示「选择连接」。 |
| A5 | 新增 `ErrorKind{None, Notice, Failure(RetryAction)}`：门禁/取消/过期 → `Notice`（保留横幅，无动作）；任务丢失（`finish_lost`）与清单读取失败 → `Failure`，用 `kit::error_state_with_action` + 「重试」（`common.retry`，`RefreshCw`）。重试按 `RetryAction{Choices,Structure,Data}` 只重发失败的那一步，不动两侧选择。错误标题：清单失败用「选择连接」，比较失败用「比较没有完成。」；原因与标题同文时给下一步「请选择已打开的基准和对照。」，不留空详情。 |
| A6 | 视口窄于 `SIDES_SIDE_BY_SIDE_MIN_W = 940`（= 两侧各 340 可用宽 + 分栏间距 + docked 侧栏 234）时两侧竖排，交换按钮居中；宽屏保持并排且交换按钮与卡片头对齐。页面水平边距改用 `Metrics::page_pad_for`，640 收到紧凑档。 |

## 保持不变的边界（回归约束）

- 比较请求、引擎门禁、schema 门禁、选择表/唯一键、过期结果与取消标记都没改；
- 脚本仍只经 `pending_place → confirm → take_place` 进查询编辑器，不执行（既有端到端用例继续断言目标库未被改写）；
- `compare-*` 命令 id 全部保留，新增的只有 `compare-retry` 与 `compare-guide-start`（后者是控件 id，不是命令）；
- `lines()`（对比页的文字投影，外壳与测试据此读屏）同步了新状态的文字，没有删掉任何既有可读文案。

## 仍未闭合 / 需 Owner 知道的事

- **没有新增 i18n 键**：本任务写入范围不含 `src-tauri/src/i18n/mod.rs`，所以失败态的标题、说明与重试按钮全部复用现有键（`compare.status.failed`、`compare.gates.chooseBoth`、`compare.dialog.chooseConnection`、`common.retry`）。若 Owner 希望失败原因单独成句（例如「重新读取两侧结构后再试」），需要给 compare 一段新的 i18n 文案并扩范围。
- **任务丢失时拿不到原因**：`finish_lost` 是任务板丢了结果，没有后端消息可展示，失败态只能给出“没有完成 + 下一步 + 重试”，不伪造原因。
- **窄宽阈值是视口宽度**：与 query 的 `NARROW_WIDTH`、mongo 的 `nav_collapsed` 同一做法（渲染时读 `window.viewport_size()`）。侧栏宽度可拖到 480，因此 940 在极端侧栏宽度下会略偏保守（更早竖排），不会造成溢出。
