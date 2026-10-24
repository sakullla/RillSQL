import { installSyntheticIpc } from "./synthetic-ipc.mjs";
/** Synthetic, populated native-IPC fixtures for visual inspection; never platform acceptance evidence. */
import fs from "node:fs";
import os from "node:os";
import { fileURLToPath } from "node:url";
import { chromium, expect } from "@playwright/test";

const scenarioTitles = {
  welcome: "欢迎页与资源管理器",
  connections: "连接管理",
  "connection-editor": "编辑连接",
  "connection-ssh": "SSH 隧道",
  query: "查询结果",
  "data-grid": "数据表格",
  structure: "表结构",
  definition: "函数定义",
  diagram: "关系图",
  export: "导出为 CSV",
  import: "导入 CSV",
  compare: "对比库",
  "close-guard": "关闭守卫",
  history: "查询历史",
  "history-saved": "保存的查询",
  settings: "设置",
  "command-palette": "命令面板",
  shortcuts: "键盘快捷键",
  "title-theme": "标题栏主题菜单",
};

/**
 * 场景扩容用的补充合成数据：synthetic-ipc.mjs 保持不动，这里在页面里包一层 invoke，
 * 只为现有夹具驱动不了的只读场景（函数定义、关系图、对比库、保存的查询）补命令。
 * 与 synthetic-ipc.mjs 同样仅用于界面检查，不构成任何真实行为证据。
 */
const scenarioFixtureData = {
  routine: { name: "order_summary", kind: "function", estimatedRows: null, stale: false, signature: "out total numeric", relation: null },
  definitionSql: [
    "create function public.order_summary(out total numeric, out paid_count integer)",
    "  returns setof record",
    "  language sql",
    "  as $$",
    "    select sum(total), count(*)",
    "    from public.orders",
    "    where status = 'paid';",
    "$$;",
  ].join("\n"),
  diagram: {
    database: "bookstore",
    schema: "public",
    notice: "连线只按外键整理，不代表真实布局。",
    tables: [
      { name: "customers", columns: ["id", "name", "phone"], outside: [] },
      { name: "orders", columns: ["id", "customer", "status", "total"], outside: [] },
      { name: "order_items", columns: ["id", "order_id", "quantity"], outside: [] },
    ],
    edges: [
      { name: "orders_customer_fkey", fromTable: "orders", fromColumns: ["customer"], toTable: "customers", toColumns: ["name"] },
      { name: "order_items_order_fkey", fromTable: "order_items", fromColumns: ["order_id"], toTable: "orders", toColumns: ["id"] },
    ],
    omitted: [{ name: "order_totals", message: "order_totals 是视图，不参与外键连线。" }],
  },
  savedQueries: [
    {
      id: "sq-reconcile",
      name: "日终对账",
      sql: "select id, customer, total\nfrom orders\nwhere status = 'paid'\norder by total desc;",
      connectionId: "pg-dev",
      connectionName: "书店 · 开发",
      databaseName: "bookstore",
      engine: "postgres",
      updatedAt: "2026-10-20T09:00:00.000Z",
    },
    {
      id: "sq-legacy",
      name: "旧库临时核对",
      sql: "select * from legacy_audit;",
      connectionId: "gone-db",
      connectionName: "旧报表库",
      databaseName: "reports",
      engine: "postgres",
      updatedAt: "2026-09-01T02:00:00.000Z",
    },
  ],
  compareStructure: {
    refused: null,
    script: "-- 让对照变成基准；生成时不执行。\nalter table notes add column customer text;\nalter table notes add column status text not null default 'pending';\ndrop table audit_old;\n",
    incomplete: false,
    notes: [],
    onlyBaseline: [{ kind: "表", name: "customers", detail: "只在基准里" }],
    onlyTarget: [{ kind: "表", name: "audit_old", detail: "只在对照里" }],
    changed: [{ kind: "列", name: "notes", detail: "基准比对照多 customer、status 两列" }],
    tables: [{ name: "notes", onBoth: true, needsKeyChoice: false, keys: [{ name: "notes_pkey", columns: ["id"], primary: true }] }],
  },
  compareData: {
    refused: null,
    script: "-- 数据差异脚本；生成时不执行。\nupdate notes set title = '进货核对' where id = 1;\n",
    incomplete: false,
    notes: ["两侧各读取前 100 行做对比。"],
    diffs: [{ table: "notes", action: "更新", key: "1", detail: "title：旧报表库 与 笔记 不一致" }],
  },
};

/** 在 synthetic-ipc 之上补少量命令；不改变已有命令的行为。 */
function installScenarioFixtures(data) {
  const core = window.__TAURI_INTERNALS__;
  if (!core || core.__scenarioPatched) return;
  const original = core.invoke;
  core.__scenarioPatched = true;
  core.invoke = async (command, args = {}) => {
    if (command === "catalog_page") {
      const result = await original(command, args);
      if (args?.connectionId === "pg-dev" && args?.schema === "public" && Array.isArray(result?.objects) && !result.objects.some((item) => item.name === data.routine.name)) {
        return { ...result, objects: [...result.objects, data.routine], total: (result.total ?? result.objects.length) + 1 };
      }
      return result;
    }
    if (command === "catalog_definition") return data.definitionSql;
    if (command === "diagram_read") return data.diagram;
    if (command === "query_saved_list") return data.savedQueries;
    if (command === "compare_schemas") return ["public"];
    if (command === "compare_structure") return data.compareStructure;
    if (command === "compare_data") return data.compareData;
    return original(command, args);
  };
}
const scenarioNames = Object.keys(scenarioTitles);

const option = (flag, fallback) => {
  const index = process.argv.indexOf(flag);
  if (index < 0) return fallback;
  const value = process.argv[index + 1];
  if (!value || value.startsWith("--")) throw new Error(`Missing value for ${flag}`);
  return value;
};

const themeOption = option("--theme", "all");
if (!["all", "light", "dark"].includes(themeOption)) throw new Error("Invalid --theme; use all, light or dark");
const themes = themeOption === "all" ? ["light", "dark"] : [themeOption];
const sizeOption = option("--size", "all");
const widths = sizeOption === "all" ? [1360, 900, 640] : [...new Set(sizeOption.split(",").map(Number))];
if (!widths.length || widths.some((width) => ![1360, 900, 640].includes(width))) throw new Error("Invalid --size; use 1360,900,640");
const onlyOption = option("--only", "*");
const patterns = onlyOption.split(",").map((pattern) => new RegExp(`^${pattern.split("").map((char) => (char === "*" ? ".*" : char === "?" ? "." : char.replace(/[\\^$+.[\]{}()|]/g, "\\$&"))).join("")}$`));
const selectedScenarios = scenarioNames.filter((name) => patterns.some((pattern) => pattern.test(name)));
if ((process.argv.includes("--list") || process.argv.includes("--only")) && !selectedScenarios.length) throw new Error("No matching scenarios");
if (process.argv.includes("--list")) {
  console.log(selectedScenarios.join("\n"));
  process.exit(0);
}

const root = fileURLToPath(new URL("../", import.meta.url)).replace(/\\/g, "/").replace(/\/$/, "");
const out = process.env.RILLSQL_CAPTURE_OUT ?? `${root}/docs/verification/ui`;
fs.mkdirSync(out, { recursive: true });
const previewUrl = process.env.RILLSQL_PREVIEW_URL ?? "http://localhost:1420";
let previewReady = false;
for (let attempt = 0; attempt < 50; attempt += 1) {
  try {
    const response = await fetch(previewUrl, { signal: AbortSignal.timeout(1000) });
    if (response.ok) {
      previewReady = true;
      break;
    }
  } catch {
    /* 预览服务还在启动。 */
  }
  await new Promise((resolve) => setTimeout(resolve, 200));
}
if (!previewReady) throw new Error(`预览服务未就绪：${previewUrl}。请先启动 npm run dev 或 npm run tauri dev，再运行捕获。`);

const failures = [];
const captures = [];

function findOverflow() {
  return [...document.querySelectorAll("main,section,article,input,textarea,select,button")].filter((el) => {
    if (!el.getClientRects().length) return false;
    let { left, right } = el.getBoundingClientRect();
    for (let parent = el.parentElement; parent && parent !== document.body; parent = parent.parentElement) {
      if (["auto", "scroll", "hidden", "clip"].includes(getComputedStyle(parent).overflowX)) {
        const rect = parent.getBoundingClientRect();
        left = Math.max(left, rect.left);
        right = Math.min(right, rect.right);
      }
    }
    return right > left && (right > innerWidth + 1 || left < -1);
  }).map((el) => ({ tag: el.tagName, text: el.textContent?.slice(0, 60) }));
}

async function captureImage(page, name, theme, width, state) {
  if (!selectedScenarios.includes(name)) return;
  await page.mouse.move(0, 0);
  await page.waitForTimeout(100);
  await page.evaluate((value) => {
    document.documentElement.dataset.theme = value;
  }, theme);
  const file = `${name}-${theme}-${width}.png`;
  const path = `${out}/${file}`;
  await page.screenshot({ path });
  captures.push({ name, title: scenarioTitles[name], theme, width, height: page.viewportSize().height, path, file, state });
  const over = await page.evaluate(findOverflow);
  if (over.length) failures.push({ name, theme, width, over });
  console.log(path);
}

async function openWorkspace(page) {
  const rail = page.getByRole("navigation", { name: "主导航" });
  if ((await rail.getByRole("button", { name: "工作台" }).getAttribute("aria-current")) !== "page") {
    await rail.getByRole("button", { name: "工作台" }).click();
  }
}

async function collapseOverlay(page, width) {
  if (width > 960) return;
  const root = page.locator(".app-root");
  if (await root.evaluate((element) => element.classList.contains("sidebar-hidden"))) return;
  await page.keyboard.press("Control+b");
  await expect(root).toHaveClass(/sidebar-hidden/);
}

/** 窄宽度下树是抽屉；需要资源管理器操作时先展开，用完再折叠。 */
async function showOverlay(page, width) {
  if (width > 960) return;
  const root = page.locator(".app-root");
  if (!(await root.evaluate((element) => element.classList.contains("sidebar-hidden")))) return;
  await page.keyboard.press("Control+b");
  await expect(root).not.toHaveClass(/sidebar-hidden/);
}

/** 连接编辑对话框的高级选项默认折叠；取 SSH 字段前先展开。 */
async function expandEditorAdvanced(page) {
  const advanced = page.locator("details.ce-advanced");
  if (!(await advanced.evaluate((element) => element.open))) await advanced.locator("summary").click();
  await expect(advanced).toHaveAttribute("open", "");
}

/** 导入场景用一份临时 CSV 走真实的文件选择路径；结束后删除。 */
const importCsvDir = fs.mkdtempSync(`${os.tmpdir()}/rillsql-capture-`);
const importCsvPath = `${importCsvDir}/orders-new.csv`;
fs.writeFileSync(importCsvPath, "id,customer,status,total,note,created_at\n1004,陈溯,paid,88.00,,2026-10-20 10:00\n1005,许青,pending,12.50,加急,2026-10-21 18:30\n", "utf8");

const browser = await chromium.launch();
try {
  for (const width of widths) {
    for (const theme of themes) {
      const page = await browser.newPage({ viewport: { width, height: 1000 } });
      page.on("pageerror", (error) => failures.push({ type: "pageerror", theme, width, message: error.message }));
      await page.emulateMedia({ colorScheme: theme });
      await page.addInitScript(installSyntheticIpc, theme);
      await page.addInitScript(installScenarioFixtures, scenarioFixtureData);
      try {
        await page.goto(previewUrl);
        await page.getByRole("button", { name: "新建查询", exact: true }).waitFor();
        await expect(page.locator(".desk-db")).toHaveCount(0);
        await captureImage(page, "welcome", theme, width, "已有连接时内容区不列连接卡片，对象树仍是连接名单");

        await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "连接" }).click();
        await page.getByRole("heading", { name: "连接管理" }).waitFor();
        const postgres = page.locator("article.conn-card", { hasText: "书店 · 开发" });
        const sqlite = page.locator("article.conn-card", { hasText: "笔记" });
        if (await postgres.getByRole("button", { name: "连接", exact: true }).count()) await postgres.getByRole("button", { name: "连接", exact: true }).click();
        if (await sqlite.getByRole("button", { name: "连接", exact: true }).count()) await sqlite.getByRole("button", { name: "连接", exact: true }).click();
        await expect(postgres).toContainText("已连接");
        await expect(sqlite).toContainText("已连接");
        await expect(page.locator("article.conn-card", { hasText: "库存 · 本地" })).toContainText("未连接");
        await captureImage(page, "connections", theme, width, "已连接、每次询问密码、SQLite 文件三种卡片");

        await postgres.getByRole("button", { name: "书店 · 开发 的更多操作" }).click();
        await postgres.getByRole("button", { name: "编辑配置…" }).click();
        await page.getByRole("heading", { name: /编辑连接/ }).waitFor();
        await expect(page.locator("[data-field='host']")).toHaveValue("db.internal");
        await captureImage(page, "connection-editor", theme, width, "直连必需项，高级选项按此连接展开");
        await expandEditorAdvanced(page);
        await page.locator("[data-field='sshHost']").scrollIntoViewIfNeeded();
        await expect(page.locator("[data-field='sshHost']")).toHaveValue("bastion.internal");
        await captureImage(page, "connection-ssh", theme, width, "单跳隧道和已信任的主机密钥");
        await page.locator("dialog[open]").getByRole("button", { name: "关闭", exact: true }).click();
        await page.getByRole("heading", { name: /编辑连接/ }).waitFor({ state: "hidden" });

        await openWorkspace(page);

        await collapseOverlay(page, width);
        await page.getByRole("button", { name: "新建查询" }).click();
        await collapseOverlay(page, width);
        await page.locator("[data-action='target']").click();
        await page.getByRole("listbox", { name: "执行目标" }).getByRole("option", { name: /书店 · 开发.*bookstore/ }).click();
        await page.locator(".cm-content").click();
        await page.keyboard.insertText("select id, customer, status, total\nfrom orders\nwhere status <> 'cancelled';");
        await page.locator("[data-action='run']").click();
        await expect(page.getByLabel("执行结果")).toContainText("林夏");
        await captureImage(page, "query", theme, width, "选择执行目标后的查询结果");

        await page.getByRole("button", { name: "打开命令面板" }).click();
        const palette = page.getByRole("dialog", { name: "命令面板" });
        await palette.getByRole("combobox").fill("orders");
        await palette.getByRole("option", { name: /^orders/ }).click();
        await collapseOverlay(page, width);
        await expect(page.getByLabel("数据表格")).toContainText("林夏");
        await captureImage(page, "data-grid", theme, width, "orders 已读取的三行，含 NULL");

        await collapseOverlay(page, width);
        await page.getByRole("tab", { name: "结构", exact: true }).click();
        await expect(page.getByLabel("字段名 1")).toHaveValue("id");
        await captureImage(page, "structure", theme, width, "orders 的字段、类型和默认值");

        await page.getByRole("tab", { name: "数据", exact: true }).click();
        await page.getByLabel("数据表格").getByRole("button", { name: "导出", exact: true }).click();
        await page.getByRole("dialog", { name: "导出为 CSV" }).waitFor();
        await expect(page.getByRole("dialog", { name: "导出为 CSV" })).toContainText("只是数据导出，不是数据库备份");
        await captureImage(page, "export", theme, width, "只导出已读取的行和全表两种范围");
        await page.getByRole("dialog", { name: "导出为 CSV" }).getByRole("button", { name: "关闭", exact: true }).click();

        // 导入：数据表格工具栏的“导入”走真实文件选择，解析后给出列映射与预览。
        await page.getByLabel("数据表格").getByRole("button", { name: "导入", exact: true }).click();
        const importDialog = page.getByRole("dialog", { name: "导入到已有表" });
        await importDialog.waitFor();
        await importDialog.getByLabel("导入文件").setInputFiles(importCsvPath);
        await expect(importDialog).toContainText("将写入 2 行");
        await captureImage(page, "import", theme, width, "CSV 列对应到 orders 字段，确认后才写入");
        await importDialog.getByRole("button", { name: "取消", exact: true }).click();
        await expect(importDialog).toHaveCount(0);

        // 函数定义：双击资源管理器里的函数，打开只读定义页签。
        if (selectedScenarios.includes("definition")) {
          await showOverlay(page, width);
          const more = page.locator('[data-action="tree-more"][data-key="s:pg-dev:bookstore:public"]');
          if (await more.count()) await more.click();
          await page.locator('[data-level="object"][data-object="order_summary"]').dblclick();
          await expect(page.locator("[data-definition-sql]")).toContainText("create function");
          await collapseOverlay(page, width);
          await captureImage(page, "definition", theme, width, "函数的只读定义与“复制到新查询”");
        }

        // 关系图：资源管理器每个 schema 下的“关系图”入口，卡片画布 + 外键连线清单。
        if (selectedScenarios.includes("diagram")) {
          await showOverlay(page, width);
          await page.locator('[data-action="open-diagram"][data-schema="public"]').click();
          await expect(page.locator("[data-diagram] [data-table-name='orders']")).toBeVisible();
          await collapseOverlay(page, width);
          await captureImage(page, "diagram", theme, width, "基表卡片与外键连线，只读");
        }

        await page.getByRole("button", { name: "打开命令面板" }).click();
        await page.getByRole("dialog", { name: "命令面板" }).waitFor();
        await captureImage(page, "command-palette", theme, width, "表、连接和命令");
        await page.keyboard.press("Escape");
        await page.locator("#main").focus();
        await page.keyboard.press("?");
        await page.getByRole("heading", { name: "键盘快捷键" }).waitFor();
        await captureImage(page, "shortcuts", theme, width, "全局、SQL 编辑器和表格快捷键");
        await page.getByRole("button", { name: "知道了", exact: true }).click();

        // 对比库：命令面板打开，基准 PG / 对照 SQLite，跑出结构差异与脚本。
        await page.getByRole("button", { name: "打开命令面板" }).click();
        const comparePalette = page.getByRole("dialog", { name: "命令面板" });
        await comparePalette.getByRole("combobox").fill("对比库");
        await comparePalette.getByRole("option", { name: /对比库/ }).click();
        const compareDialog = page.getByRole("dialog", { name: "对比库" });
        await compareDialog.waitFor();
        await compareDialog.locator('[data-side="baseline"][data-field="connection"]').selectOption("pg-dev");
        await compareDialog.locator('[data-side="baseline"][data-field="database"]').selectOption("bookstore");
        await compareDialog.locator('[data-side="baseline"][data-field="schema"]').selectOption("public");
        await compareDialog.locator('[data-side="target"][data-field="connection"]').selectOption("pg-dev");
        await compareDialog.locator('[data-side="target"][data-field="database"]').selectOption("bookstore");
        await compareDialog.locator('[data-side="target"][data-field="schema"]').selectOption("public");
        await compareDialog.getByRole("button", { name: "对比结构", exact: true }).click();
        await expect(compareDialog.locator("[data-compare-script]")).toHaveValue(/alter table/);
        await compareDialog.getByRole("button", { name: "对比数据", exact: true }).click();
        await expect(compareDialog.locator("[data-data-script]")).toHaveValue(/update notes/);
        await collapseOverlay(page, width);
        await captureImage(page, "compare", theme, width, "仅一侧拥有的表、列差异与两侧脚本");
        await compareDialog.getByRole("button", { name: "关闭", exact: true }).click();
        await expect(compareDialog).toHaveCount(0);
        await collapseOverlay(page, width);

        // 关闭守卫：带未执行草稿的查询标签关闭前先确认。
        await page.locator(".tabbar .tab-new").click();
        await collapseOverlay(page, width);
        await page.locator(".cm-content").click();
        await page.keyboard.insertText("-- 还没执行的草稿\nselect 1;");
        await page.locator(".tab.active .tab-x").click();
        await page.locator("#close-guard-title").waitFor();
        await captureImage(page, "close-guard", theme, width, "未执行草稿的标签关闭确认");
        await page.getByRole("button", { name: "离开", exact: true }).click();
        await expect(page.locator("#close-guard-title")).toHaveCount(0);

        // 标题栏主题菜单：外观按钮展开的三个选项（可选场景）。
        await page.locator("details.theme-menu > summary").click();
        await page.getByRole("menu", { name: "外观" }).waitFor();
        await captureImage(page, "title-theme", theme, width, "标题栏外观菜单：浅色 / 深色 / 跟随系统");
        await page.locator("details.theme-menu > summary").click();

        await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "查询历史" }).click();
        await expect(page.getByRole("heading", { name: "查询历史" })).toBeVisible();
        await expect(page.locator(".hist-card", { hasText: "旧报表库" })).toContainText("原连接已删除");
        await captureImage(page, "history", theme, width, "成功、失败，以及原连接已删除");

        // 保存的查询：同一页的第二个分段，含需要重选执行目标的旧连接条目。
        await page.getByRole("tab", { name: "保存的查询" }).click();
        await page.getByRole("heading", { name: "保存的查询" }).waitFor();
        await expect(page.locator(".hist-card", { hasText: "日终对账" })).toContainText("书店 · 开发");
        await expect(page.locator(".hist-card", { hasText: "旧库临时核对" })).toContainText("需要重新选择执行目标");
        await captureImage(page, "history-saved", theme, width, "保存的名称与 SQL，旧连接需要重选执行目标");

        await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设置" }).click();
        await page.getByLabel("编辑器字号").waitFor();
        await captureImage(page, "settings", theme, width, "外观、数据读取、执行与复制、本机数据和关于");
      } catch (error) {
        const calls = await page.evaluate(() => (window.__uiCalls ?? []).slice(-8).map((call) => call.command)).catch(() => []);
        failures.push({ type: "scenario", theme, width, message: error instanceof Error ? error.message : String(error), calls });
        console.error(error instanceof Error ? error.message : error);
      } finally {
        await page.close();
      }
    }
  }
} finally {
  await browser.close();
}

function writeGallery() {
  const escape = (value) => String(value).replace(/[&<>"']/g, (char) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[char]);
  const label = (item) => item.title ?? item.name;
  const scenarioOptions = [...new Map(captures.map((item) => [item.name, label(item)]))].map(([name, title]) => `<option value="${escape(name)}">${escape(title)}</option>`).join("");
  const cards = captures.map((item) => `<figure data-scenario="${escape(item.name)}" data-theme="${item.theme}" data-width="${item.width}" data-keywords="${escape(`${item.name} ${item.state} ${label(item)}`.toLowerCase())}"><a href="${escape(item.file)}" target="_blank" rel="noopener"><img loading="lazy" src="${escape(item.file)}" alt="${escape(label(item))} ${item.theme} ${item.width}"></a><figcaption>${escape(label(item))}<small>${item.theme} · ${item.width} × ${item.height}</small><small>${escape(item.state)}</small></figcaption></figure>`).join("");
  const missing = widths.flatMap((width) => themes.flatMap((theme) => selectedScenarios.filter((name) => !captures.some((item) => item.name === name && item.width === width && item.theme === theme)).map((name) => ({ name, theme, width }))));
  if (missing.length) failures.push({ type: "missing", missing });
  const manifestName = "capture-ui-manifest.json";
  fs.writeFileSync(`${out}/index.html`, `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>澄库 · RillSQL UI 捕获</title><style>
body{margin:0;background:#101114;color:#f5f2ec;font:15px system-ui}header{position:sticky;top:0;padding:20px 4vw;background:#191b20;z-index:2;border-bottom:1px solid #34363d}h1{font-size:24px;margin:0 0 8px}p{color:#b3afa6}.filters{display:flex;flex-wrap:wrap;gap:10px;align-items:end}label{display:grid;gap:5px;font-size:13px;color:#c9c6c0}select,input,button{font:inherit;padding:9px;border:1px solid #555;border-radius:8px;background:#25272d;color:#f5f2ec;max-width:100%;box-sizing:border-box}select{max-width:min(350px,80vw)}button{cursor:pointer}input{width:240px}.result{display:flex;gap:18px;flex-wrap:wrap;margin:12px 0 0}main{padding:24px 4vw;display:grid;grid-template-columns:repeat(auto-fit,minmax(min(300px,100%),1fr));gap:24px}figure{margin:0;padding:12px;border:1px solid #34363d;border-radius:12px;min-width:0}img{width:100%;height:340px;object-fit:contain;background:#070709}figcaption{padding:12px 0 0;overflow-wrap:anywhere}a{color:#b8d2ff}small{display:block;color:#b3afa6;margin-top:5px}#empty{padding:24px 4vw}[hidden]{display:none!important}</style></head><body><header><h1>澄库 · RillSQL UI 捕获</h1><p>${failures.length ? "捕获有失败，请查看清单" : "捕获完成"} · ${captures.length} 张 · 合成 IPC 数据，仅用于界面检查。</p><form class="filters" id="filters"><label>场景<select id="scenario" aria-label="场景"><option value="">全部场景</option>${scenarioOptions}</select></label><label>主题<select id="theme" aria-label="主题"><option value="">全部主题</option><option value="light">浅色 light</option><option value="dark">深色 dark</option></select></label><label>宽度<select id="width" aria-label="宽度"><option value="">全部宽度</option>${widths.map((width) => `<option>${width}</option>`).join("")}</select></label><label>关键词<input id="query" type="search" placeholder="如 连接 / 表格 / 导出" aria-label="场景或关键词"></label><button type="reset">重置筛选</button></form><div class="result"><output id="count" aria-live="polite"></output><a href="${manifestName}">捕获清单</a></div></header><p id="empty" hidden>没有匹配的图片，请调整筛选或重置。</p><main>${cards}</main><script>
const ids=['scenario','theme','width','query'];const controls=ids.map(id=>document.getElementById(id));const cards=[...document.querySelectorAll('figure')];function filter(){let count=0;const query=controls[3].value.trim().toLowerCase();for(const card of cards){card.hidden=controls.slice(0,3).some((control,index)=>control.value&&card.dataset[ids[index]]!==control.value)||!card.dataset.keywords.includes(query);if(!card.hidden)count++;}document.getElementById('count').textContent='显示 '+count+' / '+cards.length+' 张';document.getElementById('empty').hidden=count!==0;}controls.forEach(control=>control.addEventListener('input',filter));document.getElementById('filters').addEventListener('submit',event=>event.preventDefault());document.getElementById('filters').addEventListener('reset',()=>setTimeout(filter,0));filter();
</script></body></html>`);
  const rows = [...new Map(captures.map((item) => [item.name, label(item)]))].map(([name, title]) => {
    const state = captures.find((item) => item.name === name)?.state ?? "";
    return `| ${title}（${state}） | ${captures.filter((item) => item.name === name).map((item) => `[${item.theme} ${item.width}](${item.file})`).join(" · ")} |`;
  });
  fs.writeFileSync(`${out}/README.md`, `# UI 捕获\n\n[浏览图片画廊](index.html) · [机器可读清单](${manifestName})\n\n按场景、主题、宽度或关键词浏览界面，支持重置、匹配数量和原图查看。\n\n执行：\n\n\`\`\`sh\nRILLSQL_PREVIEW_URL=${previewUrl} ${["node", "scripts/capture-ui.mjs", ...process.argv.slice(2)].join(" ")}\n\`\`\`\n\n\`--theme light|dark|all\`、\`--size 1360,900,640\`、\`--only 'connection-*'\` 可缩小范围，\`--list\` 列出场景。RILLSQL_CAPTURE_OUT 可指定输出目录。预览地址默认 http://localhost:1420。\n\n本次生成 ${captures.length} 张图片，${failures.length} 项检查失败。合成 IPC 数据仅用于界面检查，不构成真实数据库、SSH 或平台验收。\n\n| 场景 | 图片 |\n| --- | --- |\n${rows.join("\n")}\n`);
  fs.writeFileSync(`${out}/${manifestName}`, `${JSON.stringify({ capturedAt: new Date().toISOString(), synthetic: true, invocation: ["node", "scripts/capture-ui.mjs", ...process.argv.slice(2)].join(" "), previewUrl, matrix: { themes, widths }, selectedScenarios, missing, success: failures.length === 0 && missing.length === 0, imageCount: captures.length, captures: captures.map(({ path, ...item }) => item), failures }, null, 2)}\n`);
}

writeGallery();
fs.rmSync(importCsvDir, { recursive: true, force: true });
console.log(JSON.stringify({ imageCount: captures.length, failures }, null, 2));
if (failures.length) process.exitCode = 1;
