# P3 交互模型浏览器验证报告

- **日期**: 2026-10-18
- **HEAD**: ff8a6d1
- **分支**: feat/p3-interaction
- **预览地址**: http://localhost:1430（Vite dev，合成 IPC）
- **Spec**: S10.2 / S10.4 / R6

## 验收命令结果

| 命令 | 结果 |
| --- | --- |
| `npm run typecheck` | 通过（vue-tsc --noEmit，无错误） |
| `npm test` | 通过：49 文件 / 374 用例 |
| `RILLSQL_PREVIEW_URL=http://localhost:1430 npm run verify:workbench` | 通过：13 项 checks，`passed: true`，无 page errors |
| `RILLSQL_PREVIEW_URL=http://localhost:1430 npm run capture:ui` | 通过：84 张截图，`failures: []` |

## verify:workbench 新增断言

1. **结果网格单元格右键菜单**：第一个数据单元格 button 右键 → `[role="menu"]` 可见且含 `[data-id="copy-value"]`，Escape 关闭。
2. **Shift 扩展多选**：点选单元格后 Shift+ArrowRight → 存在 `.cell-btn.selected`，Escape 清除。
3. **侧栏复位**：`.sb-resizer` 键盘拉宽到 254 后 dblclick → `aside.sidebar` 宽度回到 234（容差 ±2）。
4. **三档宽度无页面横滚**（R6）：

| 视口 | scrollWidth | clientWidth | 结论 |
| --- | --- | --- | --- |
| 1360×900 | 1360 | 1360 | 无页面级横向滚动条 |
| 900×900 | 900 | 900 | 无页面级横向滚动条 |
| 640×900 | 640 | 640 | 无页面级横向滚动条 |

三档宽度下环境文字（`env` 标签，如书店 · 开发 的「开发」）在对象树与执行目标中持续可见（既有对象树/目标断言覆盖 + 截图证据）。

## capture:ui 新增截图

在既有场景（welcome / connections / connection-editor / connection-ssh / query / data-grid / structure / export / history / settings / command-palette / shortcuts）基础上新增：

| 场景 key | 标题 | 说明 | 矩阵 |
| --- | --- | --- | --- |
| `grid-context` | 网格单元格右键菜单 | 单元格右键菜单展开，含复制值等项 | light/dark × 1360/900/640（6 张） |
| `grid-selection` | 网格多选高亮 | Shift 扩展后的多单元格高亮 | light/dark × 1360/900/640（6 张） |

本轮全量捕获共 84 张（12 场景 × 2 主题 × 3 宽度），全部落在 `docs/verification/ui/`。画廊与清单见 `docs/verification/ui/index.html`、`capture-ui-manifest.json`。

## S10.4 手动体验清单（可脚本化部分）

| 项 | 验证方式 | 结果 |
| --- | --- | --- |
| 结果网格单元格右键出菜单可关 | verify 断言 + grid-context 截图 | 通过 |
| 拖侧栏复位（dblclick → 234） | verify 断言 | 通过 |
| 列宽拖动 | 前端单测 `column-width.test.ts`；本轮浏览器脚本未覆盖列宽 pointer 拖动 | 见已知限制 |
| 网格滚轮 / Shift 横滚 | 前端单测 + 既有 verify 滚动断言 | 通过（合成） |
| 单击/双击语义（单击选中、双击进入编辑） | 前端单测；浏览器侧多选与值查看器路径覆盖单击语义 | 部分（双击编辑未在浏览器脚本断言） |

## 已知限制

1. **合成 IPC，非真机**：所有结果均来自 `scripts/synthetic-ipc.mjs` 注入的前端行为与渲染；未在 Tauri 原生壳中运行，真实数据库、剪贴板、系统对话框未验收。
2. **ContextMenu clamp 非翻转**：菜单位置是靠近边缘 clamp，不是对侧翻转；窄视口下菜单可能贴近触发点但仍保持在视口内。
3. **列宽拖动与双击编辑单元格**：属于 pointer 级交互，本轮浏览器脚本未增加专用断言，依赖既有 Vue 单测（`column-width.test.ts`、grid 编辑用例）。
4. **CodeMirror stderr**（getClientRects）为既有噪音，非失败。
5. 环境文字可见性依赖对象树/执行目标等既有断言与截图，未单独写 `env` 元素的显式 Playwright 断言。

## 脚本改动摘要

- `scripts/verify-workbench.mjs`：在既有流程后追加右键菜单、Shift 多选、侧栏 dblclick 复位、三档宽度 scrollWidth 断言；结果写入 `docs/verification/workbench-runtime.json`。
- `scripts/capture-ui.mjs`：新增 `grid-context`、`grid-selection` 两场景（query 之后、命令面板之前采集）。
- `scripts/synthetic-ipc.mjs`：未改动。
