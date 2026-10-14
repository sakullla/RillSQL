/** Synthetic, populated native-IPC fixtures for visual inspection; never platform acceptance evidence. */
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
  export: "导出 CSV",
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

function installSyntheticIpc(initialTheme) {
  const settings = {
    theme: initialTheme,
    density: "compact",
    editorFont: 13,
    pageSize: 100,
    previewKB: 1,
    objectPage: 100,
    nullCopy: "NULL",
    typeToConfirm: true,
    confirmClose: true,
  };
  localStorage.setItem("rillsql.shell", JSON.stringify(settings));

  const object = (name, kind, estimatedRows) => ({ name, kind, estimatedRows, stale: false });
  const saved = [
    {
      id: "pg-dev",
      name: "书店 · 开发",
      engine: "postgres",
      env: "dev",
      host: "db.internal",
      port: 5432,
      database: "bookstore",
      username: "reader",
      filePath: null,
      tls: { mode: "verify", serverName: "db.internal", caMode: "system", caFile: null, clientCert: null, clientKey: null },
      ssh: { host: "bastion.internal", port: 22, username: "ops", auth: "key", keyPath: "C:\\Users\\local\\.ssh\\id_ed25519" },
      passwordSaved: true,
      sshPasswordSaved: false,
      sshHostKey: "SHA256:fixture-bastion",
    },
    {
      id: "mysql-stock",
      name: "库存 · 本地",
      engine: "mysql",
      env: "local",
      host: "127.0.0.1",
      port: 3306,
      database: "inventory",
      username: "root",
      filePath: null,
      tls: { mode: "off", serverName: "127.0.0.1", caMode: "system", caFile: null, clientCert: null, clientKey: null },
      ssh: null,
      passwordSaved: false,
      sshPasswordSaved: false,
      sshHostKey: null,
    },
    {
      id: "sqlite-notes",
      name: "笔记",
      engine: "sqlite",
      env: "local",
      host: null,
      port: null,
      database: null,
      username: null,
      filePath: "C:\\Users\\local\\notes.sqlite",
      tls: null,
      ssh: null,
      passwordSaved: false,
      sshPasswordSaved: false,
      sshHostKey: null,
    },
  ];
  const catalog = [
    {
      id: "pg-dev",
      name: "书店 · 开发",
      engine: "postgres",
      env: "dev",
      envLabel: "开发",
      envTone: "dev",
      status: "connected",
      defaultDatabase: "bookstore",
      foreignKeys: null,
      error: null,
      databases: [
        {
          name: "bookstore",
          error: null,
          schemas: [
            {
              name: "public",
              error: null,
              total: 4,
              objects: [object("customers", "table", 86), object("orders", "table", 128), object("order_items", "table", 340), object("order_totals", "view", null)],
            },
          ],
        },
      ],
    },
    {
      id: "mysql-stock",
      name: "库存 · 本地",
      engine: "mysql",
      env: "local",
      envLabel: "本地",
      envTone: "local",
      status: "disconnected",
      defaultDatabase: "inventory",
      foreignKeys: null,
      error: null,
      databases: [],
    },
    {
      id: "sqlite-notes",
      name: "笔记",
      engine: "sqlite",
      env: "local",
      envLabel: "本地",
      envTone: "local",
      status: "connected",
      defaultDatabase: "main",
      foreignKeys: true,
      error: null,
      databases: [{ name: "main", error: null, schemas: [{ name: "", error: null, total: 1, objects: [object("notes", "table", 12)] }] }],
    },
  ];
  const orderColumns = [
    { name: "id", typeName: "integer", nullable: false, primary: true, unique: true, auto: true },
    { name: "customer", typeName: "text", nullable: true, primary: false, unique: false, auto: false },
    { name: "status", typeName: "text", nullable: false, primary: false, unique: false, auto: false },
    { name: "total", typeName: "numeric", nullable: false, primary: false, unique: false, auto: false },
    { name: "note", typeName: "text", nullable: true, primary: false, unique: false, auto: false },
    { name: "created_at", typeName: "timestamp", nullable: false, primary: false, unique: false, auto: false },
  ];
  const orderRows = [
    ["1001", "林夏", "paid", "128.00", null, "2026-10-01 09:12"],
    ["1002", "周宁", "pending", "56.50", "加急", "2026-10-02 14:40"],
    ["1003", null, "cancelled", "0.00", "客户取消", "2026-10-03 11:05"],
  ];
  const noteColumns = [
    { name: "id", typeName: "integer", nullable: false, primary: true, unique: true, auto: true },
    { name: "title", typeName: "text", nullable: false, primary: false, unique: false, auto: false },
    { name: "body", typeName: "text", nullable: true, primary: false, unique: false, auto: false },
  ];
  const noteRows = [
    ["1", "进货核对", "十月第二周的到货数量还差两箱。"],
    ["2", "会员价", null],
  ];
  function rowsOf(columns, matrix, key) {
    return matrix.map((values) => {
      const cells = {};
      columns.forEach((column, index) => {
        cells[column.name] = values[index];
      });
      return { id: String(values[0]), cells, located: true, locatorColumns: [key], locatorValues: [values[0]] };
    });
  }
  function schemaFor(table) {
    if (table === "notes") {
      return { engine: "sqlite", schema: "", name: "notes", kind: "table", columns: noteColumns, key: ["id"], rows: rowsOf(noteColumns, noteRows, "id"), sql: 'select "id", "title", "body" from "notes" limit 100' };
    }
    return { engine: "postgres", schema: "public", name: "orders", kind: "table", columns: orderColumns, key: ["id"], rows: rowsOf(orderColumns, orderRows, "id"), sql: 'select "id", "customer", "status", "total", "note", "created_at" from "public"."orders" limit 100' };
  }
  const history = [
    {
      id: "h1",
      sql: "select id, customer, status, total\nfrom orders\nwhere status <> 'cancelled';",
      connectionId: "pg-dev",
      connectionName: "书店 · 开发",
      databaseName: "bookstore",
      engine: "postgres",
      startedAt: "2026-10-14T02:10:00.000Z",
      finishedAt: "2026-10-14T02:10:00.018Z",
      status: "ok",
      elapsedMs: 18,
      summary: "3 行",
    },
    {
      id: "h2",
      sql: "update orders set status = 'paid' where id = 1002;",
      connectionId: "pg-dev",
      connectionName: "书店 · 开发",
      databaseName: "bookstore",
      engine: "postgres",
      startedAt: "2026-10-13T08:22:00.000Z",
      finishedAt: "2026-10-13T08:22:00.040Z",
      status: "error",
      elapsedMs: 40,
      summary: "权限不足",
    },
    {
      id: "h3",
      sql: "select * from legacy_audit;",
      connectionId: "gone-db",
      connectionName: "旧报表库",
      databaseName: "reports",
      engine: "postgres",
      startedAt: "2026-09-02T01:00:00.000Z",
      finishedAt: "2026-09-02T01:00:01.000Z",
      status: "ok",
      elapsedMs: 1000,
      summary: "12 行",
    },
  ];
  function testReport(connection) {
    if (connection.engine === "sqlite") {
      return {
        status: "ok",
        failStage: null,
        hint: null,
        summary: "SQLite 3.46.0",
        fingerprint: null,
        algorithm: null,
        stages: [
          { kind: "file", label: "打开文件", status: "ok", message: connection.filePath },
          { kind: "schema", label: "读取 schema", status: "ok", message: "1 张表" },
          { kind: "fk", label: "检查 foreign_keys", status: "ok", message: "ON" },
        ],
      };
    }
    return {
      status: "ok",
      failStage: null,
      hint: null,
      summary: "PostgreSQL 16.4",
      fingerprint: null,
      algorithm: null,
      stages: [
        { kind: "ssh-host", label: "SSH 主机身份", status: "ok", message: "主机密钥与已保存指纹一致" },
        { kind: "ssh-auth", label: "SSH 认证", status: "ok", message: "私钥认证通过" },
        { kind: "tunnel", label: "建立隧道", status: "ok", message: "已另开一条到数据库的通道" },
        { kind: "tls", label: "TLS 握手与证书验证", status: "ok", message: "证书主机名仍是 db.internal" },
        { kind: "auth", label: "数据库认证", status: "ok", message: "已登录 reader" },
        { kind: "version", label: "读取服务器版本", status: "ok", message: "PostgreSQL 16.4" },
      ],
    };
  }
  function queryResult(args) {
    const request = args.request ?? {};
    const sqlite = request.engine === "sqlite" || request.connectionId === "sqlite-notes";
    const columns = sqlite ? ["id", "title", "body"] : ["id", "customer", "status", "total"];
    const rows = sqlite ? noteRows : orderRows.slice(0, 2).map((row) => row.slice(0, 4));
    return {
      id: "run-capture",
      connectionId: request.connectionId ?? "pg-dev",
      connectionName: request.connectionName ?? "书店 · 开发",
      database: request.database ?? "bookstore",
      engine: request.engine ?? "postgres",
      status: "ok",
      cancel: "idle",
      cancelMessage: "",
      viaSsh: !sqlite,
      freshChannel: false,
      startedAt: "2026-10-14T02:10:00.000Z",
      elapsedMs: 18,
      statements: [{ sql: request.sql ?? "", status: "ok", message: "", columns, rows, truncated: false, affected: null }],
    };
  }
  const structure = {
    engine: "postgres",
    database: "bookstore",
    schema: "public",
    name: "orders",
    kind: "table",
    storage: null,
    sqliteVersion: "",
    foreignKeys: null,
    recordedForeignKeys: null,
    rowCount: 128,
    columns: [
      { name: "id", typeName: "integer", nullable: false, defaultSql: "", primary: true, auto: true, generated: false },
      { name: "customer", typeName: "text", nullable: true, defaultSql: "", primary: false, auto: false, generated: false },
      { name: "status", typeName: "text", nullable: false, defaultSql: "'pending'", primary: false, auto: false, generated: false },
      { name: "total", typeName: "numeric", nullable: false, defaultSql: "0", primary: false, auto: false, generated: false },
      { name: "note", typeName: "text", nullable: true, defaultSql: "", primary: false, auto: false, generated: false },
      { name: "created_at", typeName: "timestamp", nullable: false, defaultSql: "now()", primary: false, auto: false, generated: false },
    ],
    indexes: [
      { name: "orders_pkey", unique: true, columns: [{ name: "id", descending: false }], protected: true, reason: "主键" },
      { name: "orders_status_idx", unique: false, columns: [{ name: "status", descending: false }], protected: false, reason: "" },
    ],
    foreignKeyList: [{ name: "orders_customer_fkey", columns: ["customer"], refSchema: "public", refTable: "customers", refColumns: ["name"], onDelete: "NO ACTION", onUpdate: "NO ACTION" }],
    incoming: [],
    displaySql: "create table public.orders (\n  id integer primary key,\n  customer text,\n  status text not null default 'pending',\n  total numeric not null default 0,\n  note text,\n  created_at timestamp not null default now()\n);",
  };
  const callbacks = new Map();
  let callbackId = 0;
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener(_event, id) { callbacks.delete(id); } };
  Object.assign(window, {
    isTauri: true,
    __uiCalls: [],
    __TAURI_INTERNALS__: {
      transformCallback(callback) {
        callbackId += 1;
        callbacks.set(callbackId, callback);
        return callbackId;
      },
      unregisterCallback(id) { callbacks.delete(id); },
      runCallback(id, data) { callbacks.get(id)?.(data); },
      callbacks,
      convertFileSrc(filePath) { return filePath; },
      async invoke(command, args = {}) {
        window.__uiCalls.push({ command, args });
        if (command.startsWith("plugin:event|")) return 1;
        if (command === "plugin:dialog|open" || command === "plugin:dialog|save") return null;
        if (command === "app_info") {
          return { title: "澄库 · RillSQL", name_zh: "澄库", name_en: "RillSQL", license: "GPL-3.0-only", data_dir: "C:\\Users\\local\\AppData\\Roaming\\RillSQL" };
        }
        if (command === "catalog_sessions") return catalog.map((item) => item.id);
        if (command === "catalog_refresh") return catalog;
        if (command === "catalog_page") {
          const schema = catalog.flatMap((item) => item.databases).flatMap((item) => item.schemas).find((item) => item.name === (args.schema ?? ""));
          return { objects: schema?.objects ?? [], total: schema?.total ?? 0, error: null };
        }
        if (command === "env_tags_list") {
          return [
            { id: "dev", label: "开发", tone: "dev", position: 0 },
            { id: "test", label: "测试", tone: "test", position: 1 },
            { id: "prod", label: "生产", tone: "prod", position: 2 },
            { id: "local", label: "本地", tone: "local", position: 3 },
          ];
        }
        if (command === "env_tags_save" || command === "env_tags_delete") return command === "env_tags_save" ? { id: args.id || "tag-new", label: args.label, tone: args.tone, position: 4 } : null;
        if (command === "connections_list") return saved;
        if (command === "connections_save") {
          const draft = args.draft ?? {};
          const id = draft.id || "saved-capture";
          const next = {
            id,
            name: draft.name || draft.host || "未命名",
            engine: draft.engine,
            env: draft.env,
            host: draft.host || null,
            port: draft.port ? Number(draft.port) : null,
            database: draft.database || null,
            username: draft.username || null,
            filePath: draft.filePath || null,
            tls: null,
            ssh: draft.sshOn ? { host: draft.sshHost, port: Number(draft.sshPort || 22), username: draft.sshUser, auth: draft.sshAuth, keyPath: draft.sshKey } : null,
            passwordSaved: Boolean(draft.savePassword),
            sshPasswordSaved: false,
            sshHostKey: null,
          };
          const index = saved.findIndex((item) => item.id === id);
          if (index >= 0) saved[index] = { ...saved[index], ...next, sshHostKey: saved[index].sshHostKey };
          else saved.push(next);
          return { connection: saved.find((item) => item.id === id), note: null };
        }
        if (command === "connections_test" || command === "connections_open") {
          const connection = saved.find((item) => item.id === args.id) ?? saved[0];
          const report = testReport(connection);
          if (command === "connections_test") return report;
          return { report, connected: true, foreignKeys: connection.engine === "sqlite" ? true : null };
        }
        if (command === "connections_disconnect" || command === "connections_confirm_host_key" || command === "connections_forget_host_key" || command === "connections_delete") return command === "connections_delete" ? { connection: saved[0], databaseFileRemoved: false } : null;
        if (command === "grid_describe") {
          const schema = schemaFor(args.request?.table);
          return { engine: schema.engine, schema: schema.schema, name: schema.name, kind: schema.kind, columns: schema.columns, key: schema.key };
        }
        if (command === "grid_page") {
          const schema = schemaFor(args.request?.table);
          return { rows: schema.rows, truncated: false, sql: schema.sql };
        }
        if (command === "grid_count") return { matched: schemaFor(args.request?.table).rows.length };
        if (command === "grid_export") return { status: "ok", success: true, writtenRows: 3, path: args.request?.path ?? "orders.csv", residualPath: null, message: "已写入", partial: false };
        if (command === "grid_export_cancel" || command === "grid_commit") return command === "grid_commit" ? { applied: [], failed: null, pendingIds: [], rolledBack: false, rule: "按顺序逐条提交，遇到错误即停止；已成功的语句不会自动撤销。" } : null;
        if (command === "ddl_describe") return { ...structure, name: args.request?.table || structure.name };
        if (command === "ddl_preview") return { blocked: [], risks: [], steps: [{ sql: "select 1;", label: "预览" }], rule: "逐条执行", rebuild: false, target: "orders" };
        if (command === "ddl_execute") return { blocked: [], rule: "逐条执行", target: "orders", rebuild: false, steps: [] };
        if (command === "query_start" || command === "query_poll" || command === "query_cancel") return queryResult(args);
        if (command === "query_history") return history;
        if (command === "query_history_clear") return null;
        throw `未模拟的命令 ${command}`;
      },
    },
  });
}

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
        await page.locator("[data-object='orders']").waitFor();
        await captureImage(page, "welcome", theme, width, "已连接的 PostgreSQL 树和欢迎页");

        await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "连接" }).click();
        await page.getByRole("heading", { name: "连接管理" }).waitFor();
        const postgres = page.locator("article.conn-card", { hasText: "书店 · 开发" });
        const sqlite = page.locator("article.conn-card", { hasText: "笔记" });
        await postgres.getByRole("button", { name: "连接", exact: true }).click();
        await sqlite.getByRole("button", { name: "连接", exact: true }).click();
        await expect(postgres).toContainText("已连接");
        await expect(sqlite).toContainText("已连接");
        await expect(page.locator("article.conn-card", { hasText: "库存 · 本地" })).toContainText("未连接");
        await captureImage(page, "connections", theme, width, "已连接、未保存密码、SQLite 文件三种卡片");

        await postgres.getByRole("button", { name: "书店 · 开发 的更多操作" }).click();
        await postgres.getByRole("button", { name: "编辑配置…" }).click();
        await page.getByRole("heading", { name: /编辑连接/ }).waitFor();
        await expect(page.locator("[data-field='host']")).toHaveValue("db.internal");
        await captureImage(page, "connection-editor", theme, width, "PostgreSQL、TLS 验证和 SSH 路径");
        await page.getByRole("button", { name: /SSH 隧道/ }).click();
        await expect(page.locator("[data-field='sshHost']")).toHaveValue("bastion.internal");
        await captureImage(page, "connection-ssh", theme, width, "单跳隧道和已信任的主机密钥");
        await page.getByRole("button", { name: "关闭", exact: true }).click();
        await page.getByRole("heading", { name: /编辑连接/ }).waitFor({ state: "hidden" });

        await openWorkspace(page);
        await collapseOverlay(page, width);
        await page.locator(".wl-item", { hasText: "新建查询" }).click();
        await collapseOverlay(page, width);
        await page.getByLabel("执行目标").selectOption({ label: "书店 · 开发 / bookstore" });
        await page.locator(".cm-content").click();
        await page.keyboard.insertText("select id, customer, status, total\nfrom orders\nwhere status <> 'cancelled';");
        await page.locator("[data-action='run']").click();
        await page.getByLabel("查询结果").waitFor();
        await expect(page.getByLabel("查询结果")).toContainText("林夏");
        await captureImage(page, "query", theme, width, "选择执行目标后的查询结果");

        await page.getByRole("button", { name: "打开命令面板" }).click();
        const palette = page.getByRole("dialog", { name: "命令面板" });
        await palette.getByRole("combobox").fill("orders");
        await palette.getByRole("option", { name: /orders/ }).click();
        await collapseOverlay(page, width);
        await expect(page.getByLabel("数据表格")).toContainText("林夏");
        await captureImage(page, "data-grid", theme, width, "orders 已读取的三行，含 NULL");

        await collapseOverlay(page, width);
        await page.getByRole("tab", { name: "结构", exact: true }).click();
        await expect(page.getByLabel("字段名 1")).toHaveValue("id");
        await captureImage(page, "structure", theme, width, "orders 的字段、类型和默认值");

        await page.getByRole("tab", { name: "数据", exact: true }).click();
        await page.getByLabel("数据表格").getByRole("button", { name: "导出", exact: true }).click();
        await page.getByRole("dialog", { name: "导出 CSV" }).waitFor();
        await expect(page.getByRole("dialog", { name: "导出 CSV" })).toContainText("CSV 是数据导出，不是数据库备份。");
        await captureImage(page, "export", theme, width, "已读取行和全表两种导出范围");
        await page.getByRole("dialog", { name: "导出 CSV" }).getByRole("button", { name: "关闭", exact: true }).click();

        await page.getByRole("button", { name: "打开命令面板" }).click();
        await page.getByRole("dialog", { name: "命令面板" }).waitFor();
        await captureImage(page, "command-palette", theme, width, "表、连接和命令");
        await page.keyboard.press("Escape");
        await page.getByRole("button", { name: "快捷键与帮助" }).click();
        await page.getByRole("heading", { name: "键盘快捷键" }).waitFor();
        await captureImage(page, "shortcuts", theme, width, "全局、编辑器和表格快捷键");
        await page.getByRole("button", { name: "知道了", exact: true }).click();

        await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "查询历史" }).click();
        await expect(page.getByRole("heading", { name: "查询历史" })).toBeVisible();
        await expect(page.locator(".hist-list")).toContainText("原连接已删除");
        await captureImage(page, "history", theme, width, "成功、失败，以及原连接已删除");

        await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设置" }).click();
        await page.getByLabel("编辑器字号").waitFor();
        await captureImage(page, "settings", theme, width, "外观、读取批量和关于");
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
