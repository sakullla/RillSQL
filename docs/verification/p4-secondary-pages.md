# P4 次级页面浏览器验证报告

- **日期**: 2026-10-18
- **HEAD**: 1e31b2e（验证报告提交前）
- **分支**: feat/p4-secondary-pages
- **预览地址**: http://localhost:1430（Vite dev，合成 IPC）
- **Spec**: [S8] 次级页面、[S9] 主题密度响应式、[S10] 门禁

## 验收命令结果

| 命令 | 结果 |
| --- | --- |
| `npm run typecheck` | 通过（vue-tsc --noEmit，无错误） |
| `npm test` | 通过：52 文件 / 401 用例 |
| `RILLSQL_PREVIEW_URL=http://localhost:1430 npm run verify:workbench` | 通过：`passed: true`，无 page errors |
| `RILLSQL_PREVIEW_URL=http://localhost:1430 node scripts/capture-ui.mjs` | 通过：84 张截图，`failures: []` |

## 本期提交（`4449afe..HEAD`）

| commit | 内容 |
| --- | --- |
| `22f3c7b` | EmptyState / Switch / Segmented 底座；页面 h1 去 26px |
| `c374898` | 设置页左分区导航 + Listbox + Segmented + tsvHeaders 开关 |
| `151078f` | 导出/导入对话框 Listbox |
| `e24a354` | 结构页摘要行 + Listbox + 行高对齐 |
| `5015ddc` | 历史页密集行 + Listbox + 预览拖宽 280–520 |
| `e7f82dc` | 连接页密集行 + Listbox 筛选（含 verify/capture 定位器） |
| `24a9e14` | 欢迎区紧凑列表；树连接动作透传到连接弹窗 |
| `1e31b2e` | 表数据筛选下拉 Listbox |

## verify 与设置/连接相关的脚本调整

1. **Listbox 无 `selectOption`**：`pickListboxOption(page, label, optionName)` 打开 `button[role=combobox]` 再点 `option`。
2. **密度**：`input[name=set-density]` → `radiogroup「界面密度」` 下点「标准」。
3. **连接行**：`article.conn-card` → `article.conn-row`。
4. **R6 三档宽度**（复验）：

| 视口 | scrollWidth | clientWidth | 结论 |
| --- | --- | --- | --- |
| 1360×900 | 1360 | 1360 | 无页面级横向滚动条 |
| 900×900 | 900 | 900 | 无页面级横向滚动条 |
| 640×900 | 640 | 640 | 无页面级横向滚动条 |

## capture:ui

12 场景 × 2 主题 × 3 宽度 = **84 张**，`failures: []`。连接页截图在密集行与 `…` 菜单下弹修复后重跑通过。

## 需求抽查（不回退）

- R1：欢迎区/页头无宣传句，标题 ≤16px 级。
- R2：连接页与对象树分工未改。
- R5：历史「载入编辑器」只载入不执行。
- R6：三档宽度无页面横滚（见上表）。
- R9：连接页「没有匹配的连接」vs「连接你的第一个数据库」文案不同；历史页「没有匹配的历史记录」vs「历史为空」。
- R11：连接页底部凭据说明保留。

## 已知限制

1. **合成 IPC**：非 Tauri 真机；数据库/剪贴板/系统对话框未在原生壳验收。
2. **DataGrid 筛选 Listbox 已替换**，浏览器脚本未单列筛选下拉断言（前端单测覆盖）。
3. **历史预览拖宽**：前端单测覆盖 clamp；浏览器脚本未做 pointer 拖动断言。
4. **Context menu 视口 clamp**（P1 既有偏离）保持不变。
5. CodeMirror `getClientRects` stderr 为既有噪音。
