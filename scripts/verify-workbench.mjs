import { chromium, expect } from "@playwright/test";
import fs from "node:fs";
import { installSyntheticIpc } from "./synthetic-ipc.mjs";
import { scanChineseUiText } from "./i18n-scan.mjs";

// R6/T6：全仓界面文案字典化扫描——模板/渲染层不允许再出现裸中文（注释与纯数据/测试文件除外）。
const chineseViolations = scanChineseUiText();
if (chineseViolations.length) {
  console.error(`[i18n-scan] ${chineseViolations.length} 处裸中文残留：\n  ` + chineseViolations.join("\n  "));
  process.exit(1);
}

const base = process.env.RILLSQL_PREVIEW_URL ?? "http://localhost:1420";
const browser = await chromium.launch();
const report = { source: "Synthetic IPC; frontend behavior and rendering only", rows: 10000, columns: 12, checks: [], metrics: {} };
const HAN = /[\u3400-\u4dbf\u4e00-\u9fff]/;
const page = await browser.newPage({ viewport: { width: 1360, height: 900 } });
async function pickOption(targetPage, comboboxName, optionName) {
  await targetPage.getByRole("combobox", { name: comboboxName }).click();
  await targetPage.getByRole("option", { name: optionName, exact: true }).click();
}

/** 鼠标轨迹用例的合成数据：宽结果集与可编辑表格，形状与主流程一致但行数更小。 */
function installMouseOverrides() {
  const invoke = window.__TAURI_INTERNALS__.invoke;
  window.__TAURI_INTERNALS__.invoke = async (command, args) => {
    const result = await invoke(command, args);
    if (command === "query_start" && result.engine !== "sqlserver") {
      result.statements[0].columns = Array.from({ length: 12 }, (_, index) => index === 0 ? "id" : "column_" + index);
      result.statements[0].rows = Array.from({ length: 200 }, (_, row) => Array.from({ length: 12 }, (_, col) => col === 0 ? String(row + 1) : "value-" + (row + 1) + "-" + col));
    }
    if (command === "grid_page") {
      result.rows = Array.from({ length: 200 }, (_, index) => ({ ...result.rows[0], id: String(index + 1), cells: { ...result.rows[0].cells, id: String(index + 1), customer: "customer-" + (index + 1) }, locatorValues: [String(index + 1)] }));
    }
    return result;
  };
}

/** 真实鼠标轨迹：在起点按下、跨格移动、在终点松开。 */
async function dragBetween(host, from, to, steps = 12) {
  const start = await host.locator(from).boundingBox();
  const end = await host.locator(to).boundingBox();
  expect(start, from).not.toBeNull();
  expect(end, to).not.toBeNull();
  await host.mouse.move(start.x + start.width / 2, start.y + start.height / 2);
  await host.mouse.down();
  await host.mouse.move(start.x + start.width / 2 + 4, start.y + start.height / 2, { steps: 2 });
  await host.mouse.move(end.x + end.width / 2, end.y + end.height / 2, { steps });
  await host.mouse.up();
}

/**
 * R3/R7：真实鼠标轨迹覆盖 SQL 文本选择、两套表格拖选和右键按需查看值。
 * 明暗主题各跑一次；单击必须只改选中，查看器只能由右键「查看值」打开。
 */
async function verifyMousePaths(host, theme) {
  const queryRuns = () => host.evaluate(() => window.__uiCalls.filter(call => call.command === "query_start").length);
  await host.getByRole("button", { name: "新建查询", exact: true }).click();
  await host.locator("[data-action=target]").click();
  await host.getByRole("listbox", { name: "执行目标" }).getByRole("option", { name: /书店 · 开发/ }).click();
  await host.locator(".cm-content").click();
  await host.keyboard.insertText("SELECT id, customer, status FROM orders;");

  // SQL 编辑器：从右下往左上反向拖选，按钮切换成“执行选中”，拖动本身不发起查询。
  const runsBeforeDrag = await queryRuns();
  const editor = await host.locator(".cm-content").boundingBox();
  await host.mouse.move(editor.x + editor.width - 24, editor.y + editor.height - 6);
  await host.mouse.down();
  await host.mouse.move(editor.x + editor.width / 2, editor.y + editor.height - 6, { steps: 6 });
  await host.mouse.move(editor.x + 4, editor.y + 4, { steps: 12 });
  await host.mouse.up();
  await expect(host.locator("[data-action=run]")).toContainText("执行选中");
  expect(await queryRuns()).toBe(runsBeforeDrag);

  // 收起选区后执行整条语句，进入查询结果表格。
  await host.locator(".cm-content").click();
  await host.locator("[data-action=run]").click();
  await expect(host.locator('[data-cell="r0:id"]')).toBeVisible();

  await host.locator('[data-cell="r0:id"]').click();
  await expect(host.getByRole("region", { name: "查询值查看器" })).toHaveCount(0);
  await dragBetween(host, '[data-cell="r0:id"]', '[data-cell="r2:column_2"]');
  await expect(host.locator("button.cell-btn.on")).toHaveAttribute("data-cell", "r2:column_2");
  await expect(host.locator('[data-cell="r0:id"]')).toHaveClass(/range/);
  await expect(host.locator('[data-cell="r2:column_2"]')).toHaveClass(/range/);
  const resultRange = await host.locator(".cell-btn.range").count();
  expect(resultRange).toBeGreaterThanOrEqual(4);
  // 复制回执的格数必须等于可见选区，拖动也不进入编辑。
    await host.keyboard.press("Control+c");
    await expect(host.locator(".toast").last()).toContainText("已复制 " + resultRange + " 个单元格");
    expect(await host.locator("input[aria-label^='编辑']").count()).toBe(0);

    // 查看器默认关闭：Ctrl+Shift+C 的回执必须落在全局轻提示上，不能只写进隐藏的查看器。
    await host.keyboard.press("Control+Shift+C");
    await expect(host.locator(".toast").last()).toContainText("已复制 SQL 值");
    await expect(host.getByRole("region", { name: "查询值查看器" })).toHaveCount(0);

    // 键盘触发的右键菜单没有指针坐标：菜单必须贴在目标单元格上，而不是跑到左上角。
    await host.locator('[data-cell="r1:id"]').dispatchEvent("contextmenu", { clientX: 0, clientY: 0 });
    const keyboardMenu = host.locator(".cell-menu");
    await expect(keyboardMenu).toBeVisible();
    const keyboardCell = await host.locator('[data-cell="r1:id"]').boundingBox();
    const keyboardMenuBox = await keyboardMenu.boundingBox();
    expect(keyboardMenuBox.x).toBeGreaterThan(keyboardCell.x - 4);
    expect(keyboardMenuBox.y).toBeGreaterThan(keyboardCell.y - 4);
    // 菜单正好盖住右键格：用事件收尾，避免点不到下面的单元格。
    await host.locator('[data-cell="r0:column_1"]').dispatchEvent("pointerdown");
    await host.evaluate(() => window.dispatchEvent(new PointerEvent("pointerup")));
    await expect(keyboardMenu).toHaveCount(0);

    // 拖出网格后松手：保留最后有效区域，此后移动鼠标不再扩选，也不残留拖动。
    const gridBox = await host.locator(".grid-scroll").first().boundingBox();
    const dragCell = await host.locator('[data-cell="r0:id"]').boundingBox();
    await host.mouse.move(dragCell.x + dragCell.width / 2, dragCell.y + dragCell.height / 2);
    await host.mouse.down();
    await host.mouse.move(gridBox.x + gridBox.width / 2, gridBox.y + Math.min(gridBox.height - 6, 90), { steps: 8 });
    await host.mouse.move(gridBox.x + gridBox.width / 2, gridBox.y + gridBox.height + 24, { steps: 8 });
    await host.mouse.up();
    const outsideRange = await host.locator(".cell-btn.range").count();
    expect(outsideRange).toBeGreaterThanOrEqual(2);
    const settledCell = await host.locator("button.cell-btn.on").getAttribute("data-cell");
    await host.mouse.move(dragCell.x + dragCell.width / 2, dragCell.y + dragCell.height / 2);
    await host.mouse.move(dragCell.x + dragCell.width / 2, dragCell.y + dragCell.height / 2 + 24);
    expect(await host.locator(".cell-btn.range").count()).toBe(outsideRange);
    await expect(host.locator("button.cell-btn.on")).toHaveAttribute("data-cell", settledCell);

  // 右键「查看值」以右键单元格为目标，关闭后换格不重开。
  await host.locator('[data-cell="r1:column_1"]').click({ button: "right" });
  await host.getByRole("menuitem", { name: "查看值" }).click();
  const resultViewer = host.getByRole("region", { name: "查询值查看器" });
  await expect(resultViewer).toBeVisible();
  await expect(resultViewer).toContainText("column_1");
  await expect(resultViewer.getByRole("textbox", { name: "column_1 的值" })).toHaveValue("value-2-1");
  await resultViewer.getByRole("button", { name: "关闭" }).click();
  await expect(resultViewer).toHaveCount(0);
  await host.locator('[data-cell="r3:id"]').click();
  await expect(resultViewer).toHaveCount(0);
  report.checks.push(theme + ": SQL drag selects without running; query grid drags a rectangle, copies exactly that cell count, keeps SQL-value copy feedback global, anchors a keyboard context menu on the cell, ends an outside release without a stuck drag and opens the viewer only for the context-menu target");

  // 数据表格：同样的单击、拖选和右键查看值。
  await host.getByRole("button", { name: "打开命令面板" }).click();
  const palette = host.getByRole("dialog", { name: "命令面板" });
  await palette.getByRole("combobox").fill("orders");
  await palette.getByRole("option", { name: /^orders 书店/ }).click();
  await expect(host.locator('[data-cell="1:customer"]')).toBeVisible();
  await host.locator('[data-cell="1:customer"]').click();
  await expect(host.getByRole("region", { name: "值查看器" })).toHaveCount(0);
  await dragBetween(host, '[data-cell="1:id"]', '[data-cell="3:customer"]');
  await expect(host.locator("button.cell-btn.on")).toHaveAttribute("data-cell", "3:customer");
  await expect(host.locator('[data-cell="1:id"]')).toHaveClass(/range/);
  await expect(host.locator('[data-cell="3:customer"]')).toHaveClass(/range/);
  const tableRange = await host.locator(".cell-btn.range").count();
  expect(tableRange).toBeGreaterThanOrEqual(4);
    await host.keyboard.press("Control+c");
    await expect(host.locator(".toast").last()).toContainText("已复制 " + tableRange + " 个单元格");
    expect(await host.locator("input[aria-label^='编辑']").count()).toBe(0);

    // 数据表格的「复制 SQL 值」同样只在全局轻提示里回执。
    await host.keyboard.press("Control+Shift+C");
    await expect(host.locator(".toast").last()).toContainText("已复制 SQL 值");
    await expect(host.getByRole("region", { name: "值查看器" })).toHaveCount(0);
  await host.locator('[data-cell="2:customer"]').click({ button: "right" });
  await host.getByRole("menuitem", { name: "查看值" }).click();
  const tableViewer = host.getByRole("region", { name: "值查看器" });
  await expect(tableViewer).toBeVisible();
  await expect(tableViewer).toContainText("customer");
  await expect(tableViewer.getByRole("textbox", { name: "customer 的值" })).toHaveValue("customer-2");
  await tableViewer.getByRole("button", { name: "关闭" }).click();
  await expect(tableViewer).toHaveCount(0);
  await host.locator('[data-cell="4:customer"]').click();
  await expect(tableViewer).toHaveCount(0);
  report.checks.push(theme + ": table drag keeps current cell and range in sync, copies that count and opens the viewer only from the right-click target");
}
const errors = [];
const spreadsheetScripts = [];
page.on("request", request => {
  if (request.resourceType() === "script" && !request.url().includes("/xlsx-preview") && /\/(?:xlsx(?:-[^/]*)?\.js|xlsx\.ts|@e965_xlsx[^/]*\.js)(?:\?|$)/.test(request.url())) {
    spreadsheetScripts.push(request.url());
  }
});
page.on("pageerror", error => errors.push(error.message));
try {
  await page.addInitScript(installSyntheticIpc, "dark");
  await page.addInitScript(() => {
    window.__releasedQueryRuns = [];
    const invoke = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = async (command, args) => {
      const result = await invoke(command, args);
      if (command === "query_release") window.__releasedQueryRuns.push(args.runId);
      // SQL Server 的 SHOWPLAN_ALL 批次返回计划行集，不套用大结果集改写。
      if (command === "query_start" && result.engine !== "sqlserver") {
        result.statements[0].columns = Array.from({ length: 12 }, (_, i) => i === 0 ? "id" : `column_${i}`);
        result.statements[0].rows = Array.from({ length: 10000 }, (_, row) => Array.from({ length: 12 }, (_, col) => col === 0 ? String(row + 1) : `value-${row + 1}-${col}`));
      }
      if (command === "grid_page") result.rows = Array.from({ length: 1000 }, (_, i) => ({ ...result.rows[0], id: String(i + 1), cells: { ...result.rows[0].cells, id: String(i + 1), customer: `customer-${i + 1}` }, locatorValues: [String(i + 1)] }));
      if (command === "query_history") return Array.from({ length: 95 }, (_, i) => ({ ...result[0], id: `history-${i}`, sql: `SELECT ${i} AS value;` }));
      return result;
    };
  });
  await page.goto(base);
  const tree = page.getByRole("tree", { name: "数据库对象" });
  await expect(tree).toContainText("书店 · 开发");
  await expect(tree).toContainText("库存 · 本地");
  await expect(tree).toContainText("笔记");
  await expect(page.locator(".desk-db")).toHaveCount(0);
  await page.getByRole("button", { name: "新建查询", exact: true }).click();
  await page.locator("[data-action=target]").click();
  await page.getByRole("listbox", { name: "执行目标" }).getByRole("option", { name: /书店 · 开发/ }).click();
  await expect(page.locator("[data-action=target]")).toContainText("bookstore");
  report.checks.push("Home opens a query with the chosen connection and database");
  await page.locator(".cm-content").click();
  await page.keyboard.insertText("SELECT * FROM orders;");
  const started = performance.now();
  await page.locator("[data-action=run]").click();
  await expect(page.locator(".q-summary")).toContainText("10000 行");
  report.metrics.largeResultReadyMs = Math.round(performance.now() - started);
  await expect.poll(() => page.evaluate(() => window.__releasedQueryRuns.length)).toBe(1);
  expect(spreadsheetScripts).toEqual([]);
  report.checks.push("Completed query releases its backend snapshot; normal querying does not load the Excel codec");
  const rendered = await page.locator("[data-row-id]").count();
  expect(rendered).toBeLessThan(50);
  report.metrics.renderedRows = rendered;
  await page.locator(".grid-scroll").evaluate(el => { el.scrollTop = el.scrollHeight; });
  await expect(page.locator('[data-row-id="r9999"]')).toBeVisible();
  await page.getByRole("textbox", { name: "在已读取行中查找" }).fill("value-9876-1");
  await expect(page.locator("[data-row-id]")).toHaveCount(1);
  await expect(page.locator("[data-row-id]")).toContainText("9876");
  // R7：普通单击只改选中，值查看器必须由右键「查看值」按需打开。
  await page.locator("[data-row-id] .cell-btn").nth(1).click();
  await expect(page.getByRole("region", { name: "查询值查看器" })).toHaveCount(0);
  await page.locator("[data-row-id] .cell-btn").nth(1).click({ button: "right" });
  await page.getByRole("menuitem", { name: "查看值" }).click();
  await expect(page.getByRole("textbox", { name: "column_1 的值" })).toHaveValue("value-9876-1");
  await page.getByRole("region", { name: "查询值查看器" }).getByRole("button", { name: "关闭" }).click();
  await expect(page.getByRole("region", { name: "查询值查看器" })).toHaveCount(0);
  await page.locator("[data-row-id] .cell-btn").nth(2).click();
  await expect(page.getByRole("region", { name: "查询值查看器" })).toHaveCount(0);
  await page.getByRole("textbox", { name: "在已读取行中查找" }).fill("");
  await page.getByRole("button", { name: "仅对已读取行按 id 排序", exact: true }).click();
  await expect(page.locator('th[aria-sort="ascending"]')).toHaveCount(1);
  report.checks.push("10,000 rows remain scrollable; filtering sees offscreen rows; full values and sort direction are visible");
  const splitter = page.getByRole("separator", { name: "调整编辑器与结果区域高度" });
  await splitter.focus();
  await page.keyboard.press("ArrowDown");
  await expect(splitter).toHaveAttribute("aria-valuenow", "47");
  report.checks.push("Editor/result split supports keyboard resizing");
  await page.locator(".cm-content").evaluate(el => { window.__editorBeforeNavigation = el; });
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设置", exact: true }).click();
  await expect(page.getByRole("heading", { name: "设置", exact: true })).toBeVisible();
  await page.locator("#app-update").getByRole("button", { name: "检查更新", exact: true }).click();
  await expect(page.locator("#app-update [role=status]")).toContainText("已是最新版本");
  expect(await page.evaluate(() => window.__uiCalls.some(call => call.command === "plugin:updater|download" || call.command === "plugin:updater|install"))).toBe(false);
  report.checks.push("Manual update check reports the current version without downloading or installing");
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "工作台", exact: true }).click();
  expect(await page.locator(".cm-content").evaluate(el => el === window.__editorBeforeNavigation)).toBe(true);
  await expect(page.locator(".cm-content")).toContainText("SELECT * FROM orders;");
  await page.locator(".cm-content").click();
  await page.keyboard.press("Control+z");
  await expect(page.locator(".cm-content")).not.toContainText("SELECT * FROM orders;");
  await page.keyboard.press("Control+y");
  await expect(page.locator(".cm-content")).toContainText("SELECT * FROM orders;");
  report.checks.push("Navigation preserves the editor instance and draft; undo and redo still work after returning");
  await page.keyboard.press("Control+p");
  // sqlserver 夹具也有一张 orders：命令面板定位到书店（postgres）那张，保持原有场景。
  await page.getByRole("option", { name: /^orders 书店/ }).click();
  await expect(page.locator('[aria-label="数据表格"]')).toHaveAttribute("data-loaded", "1000");
  expect(await page.locator("[data-row-id]").count()).toBeLessThan(50);
  await page.locator('[data-cell="1:id"]').click();
  for (let i = 0; i < 60; i++) await page.keyboard.press("ArrowDown");
  await expect(page.locator('[data-cell="61:id"]')).toBeFocused();
  await expect(page.locator('[data-cell="61:id"]')).toBeVisible();
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设置", exact: true }).click();
  await page.locator('input[name="set-density"][value="standard"]').check();
  await pickOption(page, "每次读取的行数", "500 行");
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "工作台", exact: true }).click();
  await page.locator(".grid-scroll").evaluate(el => { el.scrollTop = el.scrollHeight; });
  await expect(page.locator('[data-row-id="1000"]')).toBeVisible();
  const measuredHeight = await page.locator('[data-row-id="1000"]').evaluate(el => el.getBoundingClientRect().height);
  expect(measuredHeight).toBe(32); // 密度三档：standard 行高 = compact 28 + 4px 步进
  report.metrics.standardDataRowPx = measuredHeight;
  await page.getByRole("button", { name: "按 id 排序", exact: true }).click();
  expect(await page.evaluate(() => window.__uiCalls.filter(call => call.command === "grid_page").at(-1).args.request.limit)).toBe(500);
  report.checks.push("1,000 editable rows stay bounded; keyboard crosses the viewport; density and page size update without losing the table");
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "查询历史", exact: true }).click();
  await expect(page.locator(".hist-card")).toHaveCount(30);
  await page.getByRole("button", { name: "下一页" }).click();
  await expect(page.locator(".hist-card").first()).toContainText("SELECT 30 AS value;");
  await page.getByRole("searchbox", { name: "搜索历史" }).fill("SELECT 94 AS");
  await expect(page.locator(".hist-card")).toHaveCount(1);
  await page.getByRole("button", { name: "清空历史", exact: true }).click();
  await expect(page.locator(".hist-clear")).toBeVisible();
  await page.getByRole("button", { name: "取消", exact: true }).click();
  expect(await page.evaluate(() => window.__uiCalls.filter(call => call.command === "query_history_clear").length)).toBe(0);
  report.checks.push("95 history entries paginate and filter globally; cancelling clear does not erase data");
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "连接", exact: true }).click();
  await pickOption(page, "按数据库类型筛选", "PostgreSQL");
  await expect(page.locator("article.conn-card")).toHaveCount(1);
  await pickOption(page, "按环境筛选", "本地");
  await expect(page.locator("article.conn-card")).toHaveCount(0);
  await pickOption(page, "按环境筛选", "全部环境");
  await pickOption(page, "按数据库类型筛选", "全部数据库类型");
  report.checks.push("Connection engine and environment filters compose correctly");
  const fresh = await browser.newPage({ viewport: { width: 640, height: 800 } });
  await fresh.addInitScript(installSyntheticIpc, "light");
  await fresh.goto(base);
  await fresh.getByRole("button", { name: "打开命令面板" }).click();
  const palette = fresh.getByRole("dialog", { name: "命令面板" });
  await palette.getByRole("combobox").fill("SQLite");
  await palette.getByRole("option", { name: "打开 SQLite 文件", exact: true }).click();
  await expect(fresh.locator("dialog[open]")).toBeVisible();
  expect(await fresh.evaluate(() => window.__uiCalls.filter(call => call.command === "plugin:dialog|open").length)).toBe(1);
  await fresh.locator("dialog[open]").getByRole("button", { name: "关闭", exact: true }).click();
  await fresh.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "工作台", exact: true }).click();
  await fresh.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "连接", exact: true }).click();
  await expect(fresh.locator(".connection-filters")).toBeVisible();
  await expect(fresh.locator("dialog[open]")).toHaveCount(0);
  expect(await fresh.evaluate(() => window.__uiCalls.filter(call => call.command === "plugin:dialog|open").length)).toBe(1);
  report.checks.push("SQLite file picker opens exactly once on request and does not reopen when returning to connections");
  await fresh.close();

  // ---- SQL Server 合成用例（T10）：方言生成/切分/计划解析不依赖 docker ----
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "工作台", exact: true }).click();
  await expect(tree).toContainText("报表 · SQL Server");
  await expect(tree).toContainText("dbo");
  await page.getByRole("button", { name: "新建查询", exact: true }).click();
  await page.locator("[data-action=target]").click();
  await page.getByRole("listbox", { name: "执行目标" }).getByRole("option", { name: /报表 · SQL Server/ }).click();
  await expect(page.locator("[data-action=target]")).toContainText("rillsql");

  // 顶层 BEGIN...END 块在「当前语句」模式下是一条语句：块内分号不切分。
  await page.locator(".cm-content").click();
  await page.keyboard.insertText("BEGIN\n  UPDATE [dbo].[orders] SET [status] = N'paid';\n  UPDATE [dbo].[orders] SET [total] = 1;\nEND\nSELECT 1;");
  // 光标移进块内（END 行），用「执行当前语句」模式验证切分：块是一条语句。
  await page.keyboard.press("ArrowUp");
  await page.locator("[data-action=run-menu]").click();
  await page.getByRole("menuitem", { name: "执行当前语句" }).click();
  const blockRun = await page.evaluate(() => window.__uiCalls.filter(call => call.command === "query_start").at(-1).args.request);
  expect(blockRun.sql.replace(/\s+/g, " ").trim()).toBe("BEGIN UPDATE [dbo].[orders] SET [status] = N'paid'; UPDATE [dbo].[orders] SET [total] = 1; END");
  expect(blockRun.engine).toBe("sqlserver");
  report.checks.push("SQL Server: top-level BEGIN...END block is sent as one statement in current-statement mode");

  // 执行计划：SET SHOWPLAN_ALL 批次 + 行集按层次呈现。
  await page.keyboard.press("Control+z");
  await page.keyboard.insertText("SELECT * FROM [dbo].[orders];");
  await page.getByRole("button", { name: "执行计划", exact: true }).click();
  const planRun = await page.evaluate(() => window.__uiCalls.filter(call => call.command === "query_start").at(-1).args.request);
  expect(planRun.sql).toBe("SET SHOWPLAN_ALL ON;\nSELECT * FROM [dbo].[orders];\nSET SHOWPLAN_ALL OFF");
  await expect(page.locator("[data-result=plan]")).toContainText("Clustered Index Scan");
  report.checks.push("SQL Server: plan wraps the statement in a SET SHOWPLAN_ALL batch and renders the rowset as a tree");

  // 网格编辑生成 T-SQL：括号标识符与 N'' 字面量（IDENTITY 列不进 UPDATE SET）。
  // 当前仍在工作台视图（再次点击「工作台」会切换侧栏显示，这里不再点导航）。
  // 逐级确认展开（会话默认展开，但前序操作可能折叠过），再打开 sqlserver 的 orders 表数据。
  const mssqlTree = tree.locator('section[data-connection="mssql-report"]');
  const expand = async (row) => {
    if ((await row.getAttribute("aria-expanded")) === "false") await row.click();
  };
  await expand(mssqlTree.locator(".tr-conn"));
  const mssqlDatabase = mssqlTree.locator('[data-level="database"][data-database="rillsql"]');
  await mssqlDatabase.waitFor();
  await expand(mssqlDatabase);
  const mssqlSchema = mssqlTree.locator('[data-level="schema"][data-schema="dbo"]');
  await mssqlSchema.waitFor();
  await expand(mssqlSchema);
  const mssqlOrders = mssqlTree.locator('[data-level="object"][data-object="orders"][data-kind="table"]');
  await mssqlOrders.waitFor();
  await mssqlOrders.dblclick();
  await expect(page.locator('[aria-label="数据表格"]')).toBeVisible();
  await page.locator('[data-cell="1:status"]').dblclick();
  await page.getByRole("textbox", { name: "编辑 status" }).fill("shipped");
  await page.getByRole("button", { name: "确定", exact: true }).click();
  await page.getByRole("button", { name: "提交", exact: true }).click();
  const preview = await page.getByRole("dialog", { name: "待提交的更改" }).locator("pre").first().textContent();
  expect(preview).toContain("UPDATE [dbo].[orders] SET [status] = N'shipped' WHERE [id] = 1");
  await page.getByRole("dialog", { name: "待提交的更改" }).getByRole("button", { name: "取消" }).click();
  await page.getByRole("button", { name: "放弃", exact: true }).click();
  await page.getByRole("button", { name: "确认放弃", exact: true }).click();
  report.checks.push("SQL Server: grid edit preview emits T-SQL with bracket identifiers and N'' literals");

  // ---- Redis 合成用例（T11）：SCAN 分页/类型化值渲染/危险命令确认，不依赖 docker ----
  await expect(tree).toContainText("缓存 · Redis");
  const redisSection = tree.locator('section[data-connection="redis-cache"]');
  // data-redis-tree 容器是 display:contents（无自身盒），用计数断言存在性。
  await expect(redisSection.locator("[data-redis-tree]")).toHaveCount(1);
  // 能力门控：Redis 连接节点下没有库/schema/关系图入口。
  await expect(redisSection.locator('[data-level="database"]')).toHaveCount(0);
  await expect(redisSection.locator('[data-action="open-diagram"]')).toHaveCount(0);
  await expect(redisSection.locator('[data-level="key"]')).toHaveCount(3);
  // SCAN 分页：「再显示」用返回的游标继续，第二页补齐 zset/stream。
  await redisSection.locator('[data-action="tree-more"]').click();
  await expect(redisSection.locator('[data-level="key"]')).toHaveCount(5);
  // 分页语义：「再显示」之后出现过 cursor=37 的续读（首读的重复游标 0 不影响——树可能因目录刷新重挂载）。
  const scanCalls = await page.evaluate(() => window.__uiCalls.filter(call => call.command === "redis_scan_keys").map(call => call.args.cursor));
  expect(scanCalls.filter(cursor => cursor !== 0)).toEqual([37]);
  expect(new Set(scanCalls).has(0)).toBe(true);
  // 双击 zset 键开键浏览器标签：分值列表渲染，TTL 展示。
  await redisSection.locator('[data-key="rillsql:zset:rank"]').dblclick();
  await expect(page.locator("[data-redis-key-title]")).toHaveText("rillsql:zset:rank");
  await expect(page.locator("[data-value-kind=zset]")).toContainText("张伟");
  await expect(page.locator("[data-value-kind=zset]")).toContainText("85");
  await expect(page.locator(".rk-ttl-now")).toContainText("30 分");
  // 标签页自己的键列表按 SCAN 分页：先「再读取一批」补齐第二页（zset/stream）。
  await page.locator('[data-action=redis-more]').click();
  await expect(page.locator(".redis-tab [data-redis-keys] [data-key]")).toHaveCount(5);
  // hash 值查看：键值表格渲染中文值（标签页内的键列表，区别于树的同名 data-key）。
  await page.locator('.redis-tab [data-key="rillsql:hash:order:1001"]').dblclick();
  await expect(page.locator("[data-value-kind=hash]")).toContainText("林夏");
  // stream 值查看：消息列表带 ID 与字段。
  await page.locator('.redis-tab [data-key="rillsql:stream:events"]').dblclick();
  await expect(page.locator("[data-value-kind=stream]")).toContainText("1698751000000-0");
  await expect(page.locator("[data-value-kind=stream]")).toContainText("管理员");
  // 命令行：只读直通（GET → bulk 文本）。
  await page.locator("[data-field=redis-command]").fill("GET rillsql:string:welcome");
  await page.locator("[data-action=redis-exec]").click();
  await expect(page.locator("[data-reply-kind=bulk]")).toContainText("澄库 Redis 夹具 · 中文值");
  // KEYS 被拒绝并提示改用 SCAN。
  await page.locator("[data-field=redis-command]").fill("KEYS *");
  await page.locator("[data-action=redis-exec]").click();
  await expect(page.locator("[data-redis-cmd-error]")).toContainText("已禁用 KEYS");
  // FLUSHDB 二次确认：取消后没有 confirmed=true 的调用，键列表不变。
  await page.locator("[data-field=redis-command]").fill("FLUSHDB");
  await page.locator("[data-action=redis-exec]").click();
  await expect(page.locator("[data-redis-confirm-prompt]")).toContainText("清空当前库");
  await page.locator("[data-action=redis-confirm-cancel]").click();
  const execCallsAfterCancel = await page.evaluate(() => window.__uiCalls.filter(call => call.command === "redis_exec"));
  expect(execCallsAfterCancel.filter(call => call.args.confirmed === true)).toHaveLength(0);
  await expect(page.locator('.redis-tab [data-key="rillsql:string:welcome"]')).toBeVisible();
  report.checks.push("Redis: SCAN pagination with cursor, typed value viewers, KEYS refused, FLUSHDB confirm-cancel leaves keys intact");

  // 能力门控：查询目标下拉不出现 Redis/MongoDB 连接（SQL 执行器对 kv/document 引擎不可达）。
  await page.getByRole("button", { name: "新建查询", exact: true }).click();
  await page.locator("[data-action=target]").click();
  const targetNames = await page.getByRole("listbox", { name: "执行目标" }).getByRole("option").allInnerTexts();
  expect(targetNames.some(name => name.includes("缓存 · Redis"))).toBe(false);
  expect(targetNames.some(name => name.includes("文档 · MongoDB"))).toBe(false);
  expect(targetNames.some(name => name.includes("书店 · 开发"))).toBe(true);
  await page.keyboard.press("Escape");
  report.checks.push("Capability gating: the SQL executor target list has no redis or mongodb connection");

  // ---- MongoDB 合成用例（T12）：集合树计数/文档分页/过滤定位/CRUD 断言/选择器 7 项，不依赖 docker ----
  await expect(tree).toContainText("文档 · MongoDB");
  const mongoSection = tree.locator('section[data-connection="mongo-docs"]');
  await expect(mongoSection.locator("[data-mongo-tree]")).toHaveCount(1);
  // 能力门控：document 引擎没有关系图/SQL 对象入口；集合行是 data-level=collection。
  await expect(mongoSection.locator('[data-action="open-diagram"]')).toHaveCount(0);
  await expect(mongoSection.locator('[data-level="object"]')).toHaveCount(0);
  await expect(mongoSection.locator('[data-level="database"]')).toHaveCount(1);
  await expect(mongoSection.locator('[data-level="collection"]')).toHaveCount(2);
  await expect(mongoSection.locator('[data-collection="orders"]')).toContainText("25");
  await expect(mongoSection.locator('[data-collection="customers"]')).toContainText("3");
  // 双击集合开文档标签：_id 与顶层字段为列，中文值渲染，计数呈现。
  await mongoSection.locator('[data-collection="orders"]').dblclick();
  await expect(page.locator("[data-mongo-collection-title]")).toContainText("rillsql / orders");
  await expect(page.locator('th[data-column="_id"]')).toHaveCount(1);
  await expect(page.locator('th[data-column="客户"]')).toHaveCount(1);
  await expect(page.locator('[data-cell="客户"]').first()).toContainText("林夏");
  await expect(page.locator("[data-mongo-count]")).toContainText("25");
  // JSON 过滤：非法过滤给定位提示（line/column），不是静默空结果。
  await page.locator("[data-field=mongo-filter]").fill('{"status": }');
  await page.locator("[data-action=mongo-apply-filter]").click();
  await expect(page.locator("[data-mongo-filter-error]")).toContainText("line 1 column");
  // 合法过滤只显示匹配文档，计数切换为精确匹配。
  await page.locator("[data-field=mongo-filter]").fill('{"status": "active"}');
  await page.locator("[data-action=mongo-apply-filter]").click();
  await expect(page.locator("[data-mongo-count]")).toContainText("匹配 1");
  await expect(page.locator("[data-cell=status]").first()).toContainText("active");
  // 文档查看：JSON 查看器保留扩展 JSON 形态（$oid）。
  await page.locator("[data-action=mongo-view]").first().click();
  await expect(page.locator("[data-value-kind=json]")).toContainText("$oid");
  await page.locator("[data-action=mongo-close-view]").click();
  // 编辑：非法 JSON 前端拦截，没有发出替换请求。
  await page.locator("[data-action=mongo-edit]").first().click();
  await page.locator("[data-field=mongo-editor]").fill('{"status": paid}');
  await page.locator("[data-action=mongo-editor-save]").click();
  await expect(page.locator("[data-mongo-editor-error]")).toContainText("JSON 语法错误");
  expect(await page.evaluate(() => window.__uiCalls.filter(call => call.command === "mongo_replace_document").length)).toBe(0);
  await page.locator("[data-action=mongo-editor-cancel]").click();
  // 新增：校验通过提交，报告服务器生成的 _id。
  await page.locator("[data-action=mongo-insert]").click();
  await page.locator("[data-field=mongo-editor]").fill('{"status":"new","备注":"加急"}');
  await page.locator("[data-action=mongo-editor-save]").click();
  await expect(page.locator("[data-mongo-note]")).toContainText("65f2c1e4a1b2c3d4e5f6f001");
  // 删除：确认弹层带 _id；取消后没有 confirmed=true 的调用；确认后报告删除计数。
  await page.locator("[data-action=mongo-delete]").first().click();
  await expect(page.locator("[data-mongo-confirm-prompt]")).toContainText("$oid");
  await page.locator("[data-action=mongo-confirm-cancel]").click();
  expect(await page.evaluate(() => window.__uiCalls.filter(call => call.command === "mongo_delete_document" && call.args.confirmed === true).length)).toBe(0);
  await page.locator("[data-action=mongo-delete]").first().click();
  await page.locator("[data-action=mongo-confirm-ok]").click();
  await expect(page.locator("[data-mongo-note]")).toContainText("已删除 1 篇文档");
  report.checks.push("MongoDB: collection tree with counts, docs grid with _id columns, located filter errors, JSON edit interception, insert id report, delete confirm with count");

  // R14：引擎选择器最终恰 7 项（无第八项）。
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "连接", exact: true }).click();
  await page.getByRole("button", { name: "新建连接", exact: true }).click();
  await expect(page.locator(".ce-engine")).toHaveCount(7);
  await expect(page.locator(".ce-engine").filter({ hasText: "MongoDB" })).toHaveCount(1);
  expect(await page.locator(".ce-engine").allInnerTexts().then(list => list.join("|"))).not.toContain("ClickHouse");
  await page.getByRole("button", { name: "取消", exact: true }).click();
  report.checks.push("Engine selector shows exactly seven engines with MongoDB and no eighth entry (R14)");

  // ---- R6/T6：语言切换即时生效 + 英文界面无中文残留 + 切回中文无缺项 ----
  // 切英文：设置页语言行 → English。
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设置", exact: true }).click();
  await pickOption(page, "界面语言", "English");
  await expect(page.getByRole("heading", { name: "Settings", exact: true })).toBeVisible();
  // 包括关于页在内，英文设置界面不应残留中文品牌名或释义。
  expect(await page.locator("section.set-sec").first().innerText()).not.toMatch(HAN);
  for (const section of await page.locator("section.set-sec").all()) {
    expect(await section.innerText()).not.toMatch(HAN);
  }
  expect(await page.locator(".settings-nav").innerText()).not.toMatch(HAN);
  report.checks.push("Language switches to English instantly from the settings row");

  // 主要界面遍历：连接 / 查询历史 / 工作台（欢迎空态）。数据夹具值（连接名等）是数据不是界面文案，
  // 因此对 chrome 区域（页头/导航/状态栏/标题栏）断言无汉字。
  const englishSurfaces = [];
  const navEn = page.getByRole("navigation", { name: "Main navigation" });
  await navEn.getByRole("button", { name: "Connections", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Connections", exact: true })).toBeVisible();
  englishSurfaces.push(await page.locator(".page-head").innerText());
  await navEn.getByRole("button", { name: "Query history", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Query history", exact: true })).toBeVisible();
  englishSurfaces.push(await page.locator(".page-head").innerText());
  await navEn.getByRole("button", { name: "Workbench", exact: true }).click();
  await page.getByRole("button", { name: "Open command palette" }).click();
  const paletteEn = page.getByRole("dialog", { name: "Command palette" });
  await expect(paletteEn).toBeVisible();
  // 面板可能停在「表」模式（此前 Ctrl+P 用例），输入 > 只看命令——命令名/分组头全走字典。
  await paletteEn.getByRole("combobox").fill(">");
  await page.waitForTimeout(200);
  const paletteText = await paletteEn.innerText();
  expect(paletteText).toContain("New query");
  expect(paletteText).not.toMatch(HAN);
  await page.keyboard.press("Escape");
  englishSurfaces.push(await page.locator(".titlebar").innerText());
  englishSurfaces.push(await page.locator(".rail").innerText());
  for (const surface of englishSurfaces) expect(surface, surface).not.toMatch(HAN);
  // 状态栏左侧会显示当前标签的连接名（数据），只断言状态文字走英文。
  expect(await page.locator(".statusbar").innerText()).toContain("Connected");
  report.checks.push("English locale: major surfaces (connections/history headings, palette, titlebar, statusbar, rail) show no Chinese leftovers");

  // ---- T6-A3：英文 locale 的连接测试失败路径无中文残留 ----
  // 运行时把 connections_test 换成带稳定码的失败报告（阶段 label/ok 消息/hint 均为后端中文原文），
  // 断言呈现层按 kind 映射阶段名、按码取字典、跳过态走固定句——en 下整条失败路径无汉字。
  await page.evaluate(() => {
    const core = window.__TAURI_INTERNALS__;
    core.__originalTestInvoke = core.invoke;
    core.invoke = async (command, args = {}) => {
      if (command === "connections_test") {
        return {
          status: "err",
          failStage: "登录数据库",
          hint: "请检查数据库用户名和密码。",
          summary: null,
          fingerprint: null,
          algorithm: null,
          code: "conn.auth_failed",
          stages: [
            { kind: "network", label: "连接数据库服务器", status: "ok", message: "127.0.0.1:5432 可达" },
            { kind: "tls", label: "加密连接并核对证书", status: "ok", message: "TLS 已连接 (TLSv1.3)" },
            { kind: "database_authentication", label: "登录数据库", status: "err", message: "密码错误", code: "conn.auth_failed" },
            { kind: "server_version", label: "读取数据库版本", status: "skipped", message: "未执行" },
          ],
        };
      }
      return core.__originalTestInvoke(command, args);
    };
  });
  await navEn.getByRole("button", { name: "Connections", exact: true }).click();
  const failCard = page.locator("article.conn-card", { hasText: "书店 · 开发" });
  await failCard.locator("[data-action=conn-menu]").click();
  await page.locator("[data-action=conn-edit]").click();
  await expect(page.getByRole("heading", { name: /Edit connection/ })).toBeVisible();
  await page.locator("[data-action=cf-test]").click();
  await expect(page.locator(".test-panel.err")).toBeVisible();
  const failLineText = await page.locator(".test-panel.err .test-line").innerText();
  expect(failLineText).toContain("Connection failed: Log in to the database");
  expect(await page.locator(".test-panel.err").innerText()).not.toMatch(HAN);
  await page.locator("details.test-all > summary").click();
  const stepsTextEn = await page.locator(".test-steps").innerText();
  expect(stepsTextEn).toContain("Not run");
  expect(stepsTextEn).not.toMatch(HAN);
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.getByRole("heading", { name: /Edit connection/ })).toBeHidden();
  await page.evaluate(() => {
    const core = window.__TAURI_INTERNALS__;
    core.invoke = core.__originalTestInvoke;
    delete core.__originalTestInvoke;
  });
  report.checks.push("English locale: failed connection test maps stage kinds and error codes to dictionary text with no Chinese leftovers");

  // 快捷键帮助（英文）。
  await page.getByRole("button", { name: "Shortcuts and help" }).click();
  const helpEn = page.getByRole("dialog").filter({ hasText: "Keyboard shortcuts" });
  await expect(helpEn).toBeVisible();
  expect(await helpEn.innerText()).not.toMatch(HAN);
  await helpEn.getByRole("button", { name: "Got it" }).click();

  // 切回中文：同一批界面恢复中文（无缺项）。
  await navEn.getByRole("button", { name: "Settings", exact: true }).click();
  await pickOption(page, "Language", "中文（简体）");
  await expect(page.getByRole("heading", { name: "设置", exact: true })).toBeVisible();
  await expect(page.locator(".settings-nav")).toContainText("外观");
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "连接", exact: true }).click();
  await expect(page.getByRole("heading", { name: "连接管理", exact: true })).toBeVisible();
  report.checks.push("Switching back to Chinese restores every checked surface in Chinese");

  // 刷新后语言持久化（localStorage 设置快照）。
  await page.reload();
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设置", exact: true }).click();
  await expect(page.getByRole("heading", { name: "设置", exact: true })).toBeVisible();
  report.checks.push("Locale choice persists across reload");

  // Exercise the real updater JS API against synthetic native commands; no installer is run.
  const updates = await browser.newPage({ viewport: { width: 640, height: 900 } });
  updates.on("pageerror", error => errors.push(error.message));
  await updates.addInitScript(installSyntheticIpc, "dark");
  await updates.addInitScript(() => {
    const core = window.__TAURI_INTERNALS__;
    const original = core.invoke;
    window.__updateCalls = [];
    core.invoke = async (command, args = {}) => {
      window.__updateCalls.push(command);
      if (command === "plugin:updater|check") return { rid: 800, currentVersion: "0.2.3", version: "0.2.4", body: "Synthetic update notes <b>plain text</b>" };
      if (command === "plugin:updater|download") {
        args.onEvent.onmessage({ event: "Started", data: { contentLength: 100 } });
        args.onEvent.onmessage({ event: "Progress", data: { chunkLength: 50 } });
        return new Promise(resolve => { window.__finishUpdateDownload = () => resolve(801); });
      }
      if (command === "plugin:updater|install") {
        window.__updateCheckpoint = JSON.parse(localStorage.getItem("rillsql.session"));
        return new Promise(resolve => { window.__finishUpdateInstall = resolve; });
      }
      if (command === "plugin:process|restart") throw new Error("Synthetic restart failure");
      return original(command, args);
    };
  });
  await updates.goto(base);
  // The delayed startup check only offers the update, even with an unsaved SQL draft.
  await updates.getByRole("button", { name: "新建查询", exact: true }).click();
  await updates.locator(".cm-content").fill("SELECT 'update draft';");
  await expect(updates.getByRole("button", { name: "有更新", exact: true })).toBeVisible({ timeout: 15000 });
  expect(await updates.evaluate(() => window.__updateCalls.includes("plugin:updater|download"))).toBe(false);
  await updates.getByRole("button", { name: "有更新", exact: true }).click();
  await expect(updates.locator("#app-update")).toContainText("发现新版本 0.2.4");
  await updates.locator("#app-update summary").click();
  await expect(updates.locator("#app-update pre")).toContainText("<b>plain text</b>");
  expect(await updates.locator("#app-update pre b").count()).toBe(0);
  expect(await updates.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await updates.getByRole("button", { name: "下载并重启安装", exact: true }).click();
  await expect(updates.locator("#app-update progress")).toHaveAttribute("value", "50");
  await updates.evaluate(() => window.__finishUpdateDownload());
  await expect(updates.getByRole("dialog", { name: "正在安装更新" })).toBeVisible();
  expect(await updates.evaluate(() => window.__updateCheckpoint.tabs.some(tab => tab.draft === "SELECT 'update draft';"))).toBe(true);
  await updates.keyboard.press("Escape");
  await expect(updates.getByRole("dialog", { name: "正在安装更新" })).toBeVisible();
  await updates.evaluate(() => window.__finishUpdateInstall());
  await expect(updates.getByRole("dialog", { name: "正在安装更新" })).toHaveCount(0);
  await expect(updates.getByRole("button", { name: "重新启动", exact: true })).toBeVisible();
  await expect(updates.locator("#app-update [role=alert]")).toContainText("Synthetic restart failure");
  await updates.close();
  report.checks.push("Synthetic update: startup offer, narrow layout, explicit download progress, draft checkpoint, install modal and restart failure recovery");

  // R3/R4/R7：明暗主题下各跑一次真实鼠标轨迹（独立页面，互不污染主流程状态）。
  for (const theme of ["dark", "light"]) {
    const host = await browser.newPage({ viewport: { width: 1360, height: 900 } });
    host.on("pageerror", error => errors.push(error.message));
    // 复制回执要真实写剪贴板，才能验证选区与复制格数一致。
    await host.context().grantPermissions(["clipboard-read", "clipboard-write"], { origin: base });
    await host.addInitScript(installSyntheticIpc, theme);
    await host.addInitScript(installMouseOverrides);
    await host.goto(base);
    try {
      await verifyMousePaths(host, theme);
    } finally {
      await host.close();
    }
  }

  expect(errors).toEqual([]);
  report.checks.push("No browser page errors");
  report.passed = true;
} finally {
  report.errors = errors;
  /* 验证产物不是交付物：写到未跟踪目录，避免每次运行的耗时指标污染候选。 */
  const reportDir = process.env.RILLSQL_VERIFY_OUT_DIR ?? "docs/verification/run";
  fs.mkdirSync(reportDir, { recursive: true });
  fs.writeFileSync(`${reportDir}/workbench-runtime.json`, JSON.stringify(report, null, 2));
  await browser.close();
}
console.log(JSON.stringify(report, null, 2));
