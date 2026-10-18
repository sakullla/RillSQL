import { mount } from "@vue/test-utils";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => []),
}));
import { replaceCatalog, resetCatalog } from "../catalog/store";
import type { CatalogConnection } from "../catalog/types";
import ConnectionsPage from "../connections/ConnectionsPage.vue";
import HistoryPage from "../history/HistoryPage.vue";
import ActivityRail from "./ActivityRail.vue";
import { filterPalette, shellCommands } from "./commands";
import EnvPill from "./EnvPill.vue";
import { PRODUCT_TITLE } from "./product";
import type { ShellTab } from "./session";
import StatusBar from "./StatusBar.vue";
import { CONNECTION_STATUS_TEXT, ENVIRONMENT_LABELS, OPERATION_STATE_TEXT, type OperationState } from "./status-text";
import TitleBar from "./TitleBar.vue";
import { desktopShell } from "./window-chrome";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");

function makeTab(overrides: Partial<ShellTab> = {}): ShellTab {
  return {
    id: "q",
    kind: "query",
    title: "查询 1",
    targetText: null,
    path: null,
    draft: "",
    connectionId: null,
    connectionName: null,
    database: null,
    engine: null,
    connectionStatus: null,
    targetNotice: null,
    tablePane: "data",
    structureSection: "columns",
    ...overrides,
  };
}

describe("shell layout", () => {
  it("keeps the brand and the command entry visible", () => {
    const title = mount(TitleBar, { props: { theme: "system" } });
    expect(title.text()).toContain("RillSQL");
    expect(title.text()).toContain("澄库");
    expect(title.find(".cmd-trigger").attributes("aria-label")).toBe("打开命令面板");
    expect(desktopShell()).toBe(false);
    expect(title.find(".caption").exists()).toBe(false);
  });

  it("shows activity rail icons without visible page names", async () => {
    const labels = ["工作台", "连接", "查询历史", "设置"] as const;
    const rail = mount(ActivityRail, { props: { page: "workspace" } });
    const buttons = rail.findAll(".rail-btn");
    expect(buttons).toHaveLength(labels.length);
    for (const label of labels) expect(rail.text()).not.toContain(label);
    expect(buttons.map((button) => button.attributes("aria-label"))).toEqual([...labels]);
    expect(buttons.map((button) => button.attributes("title"))).toEqual([
      "工作台（再次点击切换资源管理器）",
      "连接",
      "查询历史",
      "设置",
    ]);
    expect(buttons.every((button) => button.find(".i").exists())).toBe(true);
    expect(buttons[0].classes()).toContain("active");
    expect(buttons[0].attributes("aria-current")).toBe("page");
    expect(buttons.slice(1).every((button) => !button.classes().includes("active"))).toBe(true);
    expect(buttons.slice(1).every((button) => button.attributes("aria-current") === "false")).toBe(true);
    await buttons[0].trigger("click");
    expect(rail.emitted("navigate")).toEqual([["workspace"]]);

    const settings = mount(ActivityRail, { props: { page: "settings" } });
    const settingsButton = settings.findAll(".rail-btn")[3];
    expect(settingsButton.classes()).toContain("active");
    expect(settingsButton.attributes("aria-current")).toBe("page");
    expect(settings.findAll(".rail-btn").slice(0, 3).every((button) => !button.classes().includes("active"))).toBe(true);
  });

  it("renders platform window controls only for custom chrome", () => {
    const caption = mount(TitleBar, { props: { theme: "system", caption: true } });
    expect(caption.get(".titlebar").attributes("data-tauri-drag-region")).toBe("deep");
    expect(caption.get(".tb-actions").attributes("data-tauri-drag-region")).toBe("false");
    expect(caption.get("[aria-label='最小化']").exists()).toBe(true);
    expect(caption.get("[aria-label='最大化']").exists()).toBe(true);
    expect(caption.get("[aria-label='关闭']").exists()).toBe(true);
  });

  it("switches theme from the title bar menu", async () => {
    const title = mount(TitleBar, { props: { theme: "system" } });
    const dark = title.findAll(".theme-menu .menu-item").find((item) => item.text().includes("深色"));
    expect(dark).toBeTruthy();
    await dark!.trigger("click");
    expect(title.emitted("set-theme")).toEqual([["dark"]]);
  });

  it("status bar keeps only the current target in plain words", () => {
    const unbound = mount(StatusBar, { props: { tab: makeTab(), onWorkspace: true } });
    expect(unbound.text()).toContain("此查询还未选择执行目标——在查询里选一个连接和库");
    // 次要入口不再常驻状态栏（ADR-4.4）
    expect(unbound.text()).not.toContain("仅存本机");
    expect(unbound.text()).not.toContain("快捷键");

    const lost = mount(StatusBar, {
      props: {
        tab: makeTab({ targetText: "Atlas / atlas_dev", connectionId: "atlas", connectionName: "Atlas", database: "atlas_dev", engine: "postgres", connectionStatus: "lost" }),
        onWorkspace: true,
      },
    });
    expect(lost.text()).toContain("Atlas / atlas_dev");
    expect(lost.text()).toContain("连接已断开");

    const table = mount(StatusBar, {
      props: { tab: makeTab({ id: "table:customers", kind: "table", title: "customers", path: "shop / public" }), onWorkspace: true },
    });
    expect(table.text()).toContain("customers");
    expect(table.text()).toContain("shop / public");

    const catalog: CatalogConnection = {
      id: "shop",
      name: "书店",
      engine: "postgres",
      env: "prod",
      envLabel: "生产",
      envTone: "prod",
      status: "connected",
      defaultDatabase: "main",
      foreignKeys: null,
      error: null,
      databases: [
        {
          name: "main",
          error: null,
          schemas: [{ name: "public", objects: [{ name: "customers", kind: "table", estimatedRows: 1, stale: false }], total: 1, error: null }],
        },
      ],
    };
    replaceCatalog([catalog]);
    const labeled = mount(StatusBar, {
      props: { tab: makeTab({ id: "table:shop:main:public:customers", kind: "table", title: "customers", path: "main / public" }), onWorkspace: true },
    });
    expect(labeled.text()).toContain("customers");
    expect(labeled.text()).toContain("main / public");
    expect(labeled.text()).toContain("生产");
    resetCatalog();
    const unlabeled = mount(StatusBar, {
      props: { tab: makeTab({ id: "table:shop:main:public:customers", kind: "table", title: "customers", path: "main / public" }), onWorkspace: true },
    });
    expect(unlabeled.text()).not.toContain("生产");

    const idle = mount(StatusBar, { props: { tab: null, onWorkspace: false } });
    expect(idle.text()).toContain(PRODUCT_TITLE);
  });

  it("drops the english eyebrows from connections, history, and settings", () => {
    const connections = mount(ConnectionsPage, { props: { sqlitePrompt: 0 } });
    expect(connections.text()).toContain("连接管理");
    expect(connections.text()).not.toContain("DATABASE CONNECTIONS");
    connections.unmount();

    const history = mount(HistoryPage);
    expect(history.text()).toContain("查询历史");
    expect(history.text()).not.toContain("QUERY HISTORY");
    expect(history.text()).not.toContain("SAVED QUERIES");
    history.unmount();

    const saved = mount(HistoryPage, { props: { section: "saved" } });
    expect(saved.text()).toContain("保存的查询");
    expect(saved.text()).not.toContain("SAVED QUERIES");
    expect(saved.text()).not.toContain("QUERY HISTORY");
    saved.unmount();
  });

  it("distinguishes the four operation states by text, not color alone", () => {
    const states: OperationState[] = ["running", "ok", "error", "unknown"];
    const labels = states.map((state) => {
      const bar = mount(StatusBar, {
        props: { tab: null, onWorkspace: false, activity: { state, text: "导出 customers.csv" } },
      });
      const activity = bar.get(".sb-activity");
      expect(activity.attributes("data-state")).toBe(state);
      expect(activity.text()).toContain(OPERATION_STATE_TEXT[state]);
      expect(activity.text()).toContain("导出 customers.csv");
      return OPERATION_STATE_TEXT[state];
    });
    expect(new Set(labels).size).toBe(4);
    expect(labels).toEqual(["进行中", "成功", "失败", "结果未知"]);

    const quiet = mount(StatusBar, { props: { tab: null, onWorkspace: false } });
    expect(quiet.find(".sb-activity").exists()).toBe(false);
  });

  it("does not distinguish environment or status by color alone", () => {
    const pill = mount(EnvPill, { props: { env: "prod", label: ENVIRONMENT_LABELS.prod } });
    expect(pill.text()).toBe("生产");
    expect(pill.classes()).toContain("env-prod");
    for (const label of Object.values(CONNECTION_STATUS_TEXT)) {
      expect(label.length).toBeGreaterThan(1);
    }
  });

  it("filters the command palette down to commands when the query starts with >", () => {
    const items = shellCommands({
      openQuery: () => undefined,
      openStructure: () => undefined,
      showPage: () => undefined,
      toggleSidebar: () => undefined,
      setTheme: () => undefined,
      openHelp: () => undefined,
      requestSqlite: () => undefined,
      saveQuery: () => undefined,
      showSavedQueries: () => undefined,
    });
    expect(items.some((item) => item.label === "查询历史")).toBe(true);
    expect(items.some((item) => item.label === "保存当前查询")).toBe(true);
    expect(items.some((item) => item.label === "保存的查询")).toBe(true);
    const commands = filterPalette(items, ">主题", "all");
    expect(commands.map((item) => item.label)).toEqual(["切换到浅色主题", "切换到深色主题", "主题跟随系统"]);
    expect(filterPalette(items, "customers", "tables")).toEqual([]);
    // 两条主路径同样能从命令面板直达
    expect(items.some((item) => item.label === "新建连接")).toBe(true);
    expect(items.some((item) => item.label === "打开 SQLite 文件")).toBe(true);
  });

  it("builds the shell styles on tokens instead of hard-coded sizes", () => {
    const css = readFileSync(resolve(root, "src/styles/shell.css"), "utf8");
    expect(css).toContain(".titlebar");
    expect(css).toContain(".caption-close");
    expect(css).toContain(".titlebar.custom-chrome");
    expect(css).toContain(".statusbar");
    expect(readFileSync(resolve(root, "src/styles/workbench.css"), "utf8")).toContain(".desk-columns");
    expect(css).toContain(".tab-target");
    expect(css).toContain("#palette");
    // 字号与动效时长一律引用令牌（ADR-3）
    expect(css).not.toMatch(/font-size:\s*[\d.]+px/);
    expect(css).not.toMatch(/[\d.]+ms/);
    const narrow = css.slice(css.indexOf("@media (max-width: 720px)"));
    expect(narrow).toContain(".rail-btn span { display: none; }");
  });

  it("puts the product title on the window and the html document", () => {
    const html = readFileSync(resolve(root, "index.html"), "utf8");
    const tauri = readFileSync(resolve(root, "src-tauri/tauri.conf.json"), "utf8");
    expect(html).toContain(`<title>${PRODUCT_TITLE}</title>`);
    expect(tauri).toContain(`"title": "${PRODUCT_TITLE}"`);
    expect(tauri).toContain(`"productName": "${PRODUCT_TITLE}"`);
  });
});
