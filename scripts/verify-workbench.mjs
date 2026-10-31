import { chromium, expect } from "@playwright/test";
import fs from "node:fs";
import { installSyntheticIpc } from "./synthetic-ipc.mjs";
const base = process.env.RILLSQL_PREVIEW_URL ?? "http://localhost:1420";
const browser = await chromium.launch();
const report = { source: "Synthetic IPC; frontend behavior and rendering only", rows: 10000, columns: 12, checks: [], metrics: {} };
const page = await browser.newPage({ viewport: { width: 1360, height: 900 } });
async function pickOption(targetPage, comboboxName, optionName) {
  await targetPage.getByRole("combobox", { name: comboboxName }).click();
  await targetPage.getByRole("option", { name: optionName, exact: true }).click();
}
const errors = [];
page.on("pageerror", error => errors.push(error.message));
try {
  await page.addInitScript(installSyntheticIpc, "dark");
  await page.addInitScript(() => {
    const invoke = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = async (command, args) => {
      const result = await invoke(command, args);
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
  expect(measuredHeight).toBe(30); // ADR-6 密度三档：standard 行高 = compact 24 + 6px 步进
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
  // 对象清单按页显示：先点「再显示」让表出现在树里。
  await mssqlTree.locator('[data-action="tree-more"]').click();
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
