import { installSyntheticIpc } from "./synthetic-ipc.mjs";
/** Synthetic, populated native-IPC fixtures for visual inspection; never platform acceptance evidence. */
// 注意：shell 工具栏与查询面板共用 data-action 值（run/format/plan）与部分可访问名称（新建查询）。
// 页面级定位器必须先用 [role="toolbar"][aria-label="主工具栏"] 或 .query-pane 限定作用域，
// 否则 Playwright strict mode 会报重名。新增控件请沿用同样的限定手法。
import fs from "node:fs";
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
  export: "导出为 CSV",
  history: "查询历史",
  settings: "设置",
  "command-palette": "命令面板",
  shortcuts: "键盘快捷键",
};
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

/** 连接编辑对话框的高级选项默认折叠；取 SSH 字段前先展开。 */
async function expandEditorAdvanced(page) {
  const advanced = page.locator("details.ce-advanced");
  if (!(await advanced.evaluate((element) => element.open))) await advanced.locator("summary").click();
  await expect(advanced).toHaveAttribute("open", "");
}

const browser = await chromium.launch();
try {
  for (const width of widths) {
    for (const theme of themes) {
      const page = await browser.newPage({ viewport: { width, height: 1000 } });
      page.on("pageerror", (error) => failures.push({ type: "pageerror", theme, width, message: error.message }));
      await page.emulateMedia({ colorScheme: theme });
      await page.addInitScript(installSyntheticIpc, theme);
      try {
        await page.goto(previewUrl);
        // P2 起主工具栏与欢迎页主路径都有「新建查询」，限定到工具栏避免 strict mode 重名。
        await page.getByRole("toolbar", { name: "主工具栏" }).getByRole("button", { name: "新建查询", exact: true }).waitFor();
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
        // 同上：作用域到主工具栏，避免与查询面板/欢迎页同名控件冲突。
        await page.getByRole("toolbar", { name: "主工具栏" }).getByRole("button", { name: "新建查询", exact: true }).click();
        await collapseOverlay(page, width);
        await page.locator("[data-action='target']").click();
        await page.getByRole("listbox", { name: "执行目标" }).getByRole("option", { name: /书店 · 开发.*bookstore/ }).click();
        await page.locator(".cm-content").click();
        await page.keyboard.insertText("select id, customer, status, total\nfrom orders\nwhere status <> 'cancelled';");
        // P2 后工具栏也有 data-action=run，限定到查询面板与 Task 7 的 verify 脚本一致。
        await page.locator(".query-pane [data-action='run']").click();
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

        await page.getByRole("button", { name: "打开命令面板" }).click();
        await page.getByRole("dialog", { name: "命令面板" }).waitFor();
        await captureImage(page, "command-palette", theme, width, "表、连接和命令");
        await page.keyboard.press("Escape");
        await page.locator("#main").focus();
        await page.keyboard.press("?");
        await page.getByRole("heading", { name: "键盘快捷键" }).waitFor();
        await captureImage(page, "shortcuts", theme, width, "全局、SQL 编辑器和表格快捷键");
        await page.getByRole("button", { name: "知道了", exact: true }).click();

        await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "查询历史" }).click();
        await expect(page.getByRole("heading", { name: "查询历史" })).toBeVisible();
        await expect(page.locator(".hist-card", { hasText: "旧报表库" })).toContainText("原连接已删除");
        await captureImage(page, "history", theme, width, "成功、失败，以及原连接已删除");

        await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设置" }).click();
        await page.getByLabel("编辑器字号").waitFor();
        await captureImage(page, "settings", theme, width, "外观、数据读取、本地数据和关于");
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
  const rows = [...new Map(captures.map((item) => [item.name, label(item)]))].map(([name, title]) => `| ${title} | ${captures.filter((item) => item.name === name).map((item) => `[${item.theme} ${item.width}](${item.file})`).join(" · ")} |`);
  fs.writeFileSync(`${out}/README.md`, `# UI 捕获\n\n[浏览图片画廊](index.html) · [机器可读清单](${manifestName})\n\n按场景、主题、宽度或关键词浏览界面，支持重置、匹配数量和原图查看。\n\n执行：\n\n\`\`\`sh\nRILLSQL_PREVIEW_URL=${previewUrl} ${["node", "scripts/capture-ui.mjs", ...process.argv.slice(2)].join(" ")}\n\`\`\`\n\n\`--theme light|dark|all\`、\`--size 1360,900,640\`、\`--only 'connection-*'\` 可缩小范围，\`--list\` 列出场景。RILLSQL_CAPTURE_OUT 可指定输出目录。预览地址默认 http://localhost:1420。\n\n本次生成 ${captures.length} 张图片，${failures.length} 项检查失败。合成 IPC 数据仅用于界面检查，不构成真实数据库、SSH 或平台验收。\n\n| 场景 | 图片 |\n| --- | --- |\n${rows.join("\n")}\n`);
  fs.writeFileSync(`${out}/${manifestName}`, `${JSON.stringify({ capturedAt: new Date().toISOString(), synthetic: true, invocation: ["node", "scripts/capture-ui.mjs", ...process.argv.slice(2)].join(" "), previewUrl, matrix: { themes, widths }, selectedScenarios, missing, success: failures.length === 0 && missing.length === 0, imageCount: captures.length, captures: captures.map(({ path, ...item }) => item), failures }, null, 2)}\n`);
}

writeGallery();
console.log(JSON.stringify({ imageCount: captures.length, failures }, null, 2));
if (failures.length) process.exitCode = 1;
