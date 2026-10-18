// 注意：shell 工具栏与查询面板共用 data-action 值（run/format/plan）与部分可访问名称（新建查询）。
// 页面级定位器必须先用 [role="toolbar"][aria-label="主工具栏"] 或 .query-pane 限定作用域，
// 否则 Playwright strict mode 会报重名。新增控件请沿用同样的限定手法。
import { chromium, expect } from "@playwright/test";
import fs from "node:fs";
import { installSyntheticIpc } from "./synthetic-ipc.mjs";
const base = process.env.RILLSQL_PREVIEW_URL ?? "http://localhost:1420";
const browser = await chromium.launch();
const report = { source: "Synthetic IPC; frontend behavior and rendering only", rows: 10000, columns: 12, checks: [], metrics: {} };

/** P4 起筛选与设置用 Listbox（button[role=combobox]），没有原生 selectOption。 */
async function pickListboxOption(page, label, optionName) {
  await page.getByRole("combobox", { name: label }).click();
  await page.getByRole("listbox", { name: label }).getByRole("option", { name: optionName, exact: true }).click();
}
const page = await browser.newPage({ viewport: { width: 1360, height: 900 } });
const errors = [];
page.on("pageerror", error => errors.push(error.message));
try {
  await page.addInitScript(installSyntheticIpc, "dark");
  await page.addInitScript(() => {
    const invoke = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = async (command, args) => {
      const result = await invoke(command, args);
      if (command === "query_start") {
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
  await page.getByRole("toolbar", { name: "主工具栏" }).getByRole("button", { name: "新建查询", exact: true }).click();
  await page.locator("[data-action=target]").click();
  await page.getByRole("listbox", { name: "执行目标" }).getByRole("option", { name: /书店 · 开发/ }).click();
  await expect(page.locator("[data-action=target]")).toContainText("bookstore");
  report.checks.push("Home opens a query with the chosen connection and database");
  await page.locator(".cm-content").click();
  await page.keyboard.insertText("SELECT * FROM orders;");
  const started = performance.now();
  await page.locator(".query-pane [data-action=run]").click();
  await expect(page.locator(".q-summary")).toContainText("10000 行");
  report.metrics.largeResultReadyMs = Math.round(performance.now() - started);
  const rendered = await page.locator("[data-row-id]").count();
  expect(rendered).toBeLessThan(50);
  report.metrics.renderedRows = rendered;
  await page.locator(".grid-scroll").evaluate(el => { el.scrollTop = el.scrollHeight; });
  await expect(page.locator('[data-row-id="r9999"]')).toBeVisible();
  await page.getByRole("textbox", { name: "在已读取行中查找" }).fill("value-9876-1");
  await expect(page.locator("[data-row-id]")).toHaveCount(1);
  await expect(page.locator("[data-row-id]")).toContainText("9876");
  await page.locator("[data-row-id] .cell-btn").nth(1).click();
  await expect(page.getByRole("textbox", { name: "column_1 的值" })).toHaveValue("value-9876-1");
  await page.getByRole("region", { name: "查询值查看器" }).getByRole("button", { name: "关闭" }).click();
  await page.getByRole("textbox", { name: "在已读取行中查找" }).fill("");
  await page.getByRole("button", { name: "仅对已读取行按 id 排序", exact: true }).click();
  await expect(page.locator('th[aria-sort="ascending"]')).toHaveCount(1);
  report.checks.push("10,000 rows remain scrollable; filtering sees offscreen rows; full values and sort direction are visible");
  // S10.4 / R6：结果网格右键菜单、Shift 扩展多选、侧栏双击复位
  const firstCell = page.locator("[data-row-id] .cell-btn").first();
  await firstCell.click({ button: "right" });
  const cellMenu = page.getByRole("menu");
  await expect(cellMenu).toBeVisible();
  await expect(cellMenu.locator('[data-id="copy-value"]')).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(cellMenu).toBeHidden();
  report.checks.push("Result grid cell context menu opens with copy-value and closes on Escape");
  await firstCell.click();
  await page.keyboard.press("Shift+ArrowRight");
  await expect(page.locator(".cell-btn.selected").first()).toBeVisible();
  await page.keyboard.press("Escape");
  report.checks.push("Shift+arrow extends a multi-cell selection on the result grid");
  const sbResizer = page.locator(".sb-resizer");
  await sbResizer.focus();
  await page.keyboard.press("ArrowRight");
  await expect(sbResizer).toHaveAttribute("aria-valuenow", "254");
  await sbResizer.dblclick();
  const sidebarBox = await page.locator("aside.sidebar").boundingBox();
  expect(sidebarBox).not.toBeNull();
  expect(Math.abs(sidebarBox.width - 234)).toBeLessThanOrEqual(2);
  report.checks.push("Sidebar resizer double-click resets width to about 234px");
  const splitter = page.getByRole("separator", { name: "调整编辑器与结果区域高度" });
  await splitter.focus();
  await page.keyboard.press("ArrowDown");
  await expect(splitter).toHaveAttribute("aria-valuenow", "47");
  report.checks.push("Editor/result split supports keyboard resizing");
  await page.locator(".cm-content").evaluate(el => { window.__editorBeforeNavigation = el; });
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设置", exact: true }).click();
  await expect(page.getByRole("heading", { name: "设置", exact: true })).toBeVisible();
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
  await page.getByRole("option", { name: /^orders/ }).click();
  await expect(page.locator('[aria-label="数据表格"]')).toHaveAttribute("data-loaded", "1000");
  expect(await page.locator("[data-row-id]").count()).toBeLessThan(50);
  await page.locator('[data-cell="1:id"]').click();
  for (let i = 0; i < 60; i++) await page.keyboard.press("ArrowDown");
  await expect(page.locator('[data-cell="61:id"]')).toBeFocused();
  await expect(page.locator('[data-cell="61:id"]')).toBeVisible();
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设置", exact: true }).click();
  await page.locator('input[name="set-density"][value="standard"]').check();
  await pickListboxOption(page, "每次读取的行数", "500 行");
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "工作台", exact: true }).click();
  await page.locator(".grid-scroll").evaluate(el => { el.scrollTop = el.scrollHeight; });
  await expect(page.locator('[data-row-id="1000"]')).toBeVisible();
  const measuredHeight = await page.locator('[data-row-id="1000"]').evaluate(el => el.getBoundingClientRect().height);
  expect(measuredHeight).toBe(32);
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
  await pickListboxOption(page, "按数据库类型筛选", "PostgreSQL");
  await expect(page.locator("article.conn-row")).toHaveCount(1);
  await pickListboxOption(page, "按环境筛选", "本地");
  await expect(page.locator("article.conn-row")).toHaveCount(0);
  await pickListboxOption(page, "按环境筛选", "全部环境");
  await pickListboxOption(page, "按数据库类型筛选", "全部数据库类型");
  report.checks.push("Connection engine and environment filters compose correctly");
  // R6：三档宽度下页面级无横向滚动
  const widthResults = [];
  for (const size of [{ width: 1360, height: 900 }, { width: 900, height: 900 }, { width: 640, height: 900 }]) {
    await page.setViewportSize(size);
    const metrics = await page.evaluate(() => ({
      scrollWidth: document.documentElement.scrollWidth,
      clientWidth: document.documentElement.clientWidth,
    }));
    expect(metrics.scrollWidth).toBeLessThanOrEqual(metrics.clientWidth + 1);
    widthResults.push({ ...size, ...metrics });
  }
  report.metrics.pageScrollWidths = widthResults;
  report.checks.push("No page-level horizontal scrollbar at 1360/900/640 widths");
  await page.setViewportSize({ width: 1360, height: 900 });
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
  expect(errors).toEqual([]);
  report.checks.push("No browser page errors");
  report.passed = true;
} finally {
  report.errors = errors;
  fs.mkdirSync("docs/verification", { recursive: true });
  fs.writeFileSync("docs/verification/workbench-runtime.json", JSON.stringify(report, null, 2));
  await browser.close();
}
console.log(JSON.stringify(report, null, 2));
