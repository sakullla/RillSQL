import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { currentCompletions, startCompletion } from "@codemirror/autocomplete";
import { redo, undo } from "@codemirror/commands";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import { nextTick, type Ref } from "vue";
import { catalogFixture } from "../catalog/fixtures";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import { registerPending, resetCloseGuard } from "../shell/close-guard";
import { insertAtCursor, resetEditors, setEditorFocus } from "../shell/query-editor";
import { shellCommands } from "../shell/commands";
import { createSession } from "../shell/session";
import { tr } from "../i18n";
import { SHORTCUTS } from "../shell/shortcuts";
import type { RunDto } from "./api";
import { clearColumnCache, qualifierFor } from "./columns";
import { completionPlan, metadataNote } from "./complete";
import { FREQUENCY_KEY, bumpIdentifiers, loadFrequency } from "./completion/frequency";
import { resolveEditor } from "./editor";
import { resetExecuteRequest } from "./execute-request";
import QueryTab from "./QueryTab.vue";
import { adoptRun, attachCloser, connectionBusy, guardClose, requestCancel, resetQueryRuntime, startTabRun, tabRun } from "./runtime";
import { QUERY_EDITOR_KEYS, sqlToRun } from "./scope";
import { inspectSql, splitSql, statementAt } from "./split";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke,
}));

function runDto(patch: Partial<RunDto> = {}): RunDto {
  return {
    id: "run-1",
    connectionId: "shop",
    connectionName: "Shop",
    database: "shop_staging",
    engine: "mysql",
    status: "done",
    cancel: "none",
    cancelMessage: "",
    viaSsh: false,
    freshChannel: false,
    statements: [],
    startedAt: "2026-10-14T00:00:00Z",
    elapsedMs: 4,
    ...patch,
  };
}

afterEach(() => {
  resetQueryRuntime();
  resetCatalog();
  clearColumnCache();
  resetCloseGuard();
  resetExecuteRequest();
  invoke.mockReset();
});

describe("query editing", () => {
  it("splits the current statement without taking the previous comment", () => {
    const sql = "SELECT 'a;b';\n-- c;\nUPDATE t SET n = 1;\nSELECT $$do;$$;";
    const parts = splitSql(sql);
    expect(parts.map((part) => part.text)).toEqual(["SELECT 'a;b';", "UPDATE t SET n = 1;", "SELECT $$do;$$;"]);
    expect(statementAt(sql, parts[1]!.start + 2)?.text).toBe("UPDATE t SET n = 1;");
    expect(sqlToRun(sql, { from: parts[1]!.start, to: parts[1]!.end }, "auto")).toEqual({ sql: "UPDATE t SET n = 1;", mode: "selection", from: parts[1]!.start, to: parts[1]!.end });
    expect(sqlToRun(sql, { from: parts[1]!.start + 2, to: parts[1]!.start + 2 }, "current").sql).toBe("UPDATE t SET n = 1;");
    expect(sqlToRun(sql, { from: 0, to: 0 }, "auto")).toEqual({ sql: "SELECT 'a;b';", mode: "current", from: parts[0]!.start, to: parts[0]!.end });
    expect(sqlToRun(sql, { from: parts[1]!.start + 2, to: parts[1]!.start + 2 }, "auto")).toEqual({ sql: "UPDATE t SET n = 1;", mode: "current", from: parts[1]!.start, to: parts[1]!.end });
  });

  it("keeps desktop editor shortcuts on Ctrl or Command", () => {
    expect(QUERY_EDITOR_KEYS).toEqual(["Mod-Enter", "Mod-Shift-Enter", "Mod-.", "Ctrl-Space", "Mod-/"]);
    expect(QUERY_EDITOR_KEYS).not.toContain("Alt-W");
    expect(QUERY_EDITOR_KEYS).not.toContain("Ctrl-Alt-N");
    expect(tr(SHORTCUTS.find((item) => item.chord === "Mod+Enter")?.label ?? "")).toBe("执行选中内容；无选中时执行光标所在语句");
    expect(tr(SHORTCUTS.find((item) => item.chord === "Mod+Shift+Enter")?.label ?? "")).toBe("只执行光标所在语句");
  });

  it("lists 执行 in the command palette with Mod+Enter", () => {
    let ran = 0;
    const item = shellCommands({
      openQuery: () => undefined,
      openStructure: () => undefined,
      showPage: () => undefined,
      toggleSidebar: () => undefined,
      setTheme: () => undefined,
      openHelp: () => undefined,
      requestSqlite: () => undefined,
      saveQuery: () => undefined,
      showSavedQueries: () => undefined,
      executeQuery: () => { ran += 1; },
    }).find((entry) => entry.label === "执行");
    expect(item?.group).toBe("command");
    expect(item?.chord).toBe("Mod+Enter");
    item?.run();
    expect(ran).toBe(1);
  });

  it("completes from the selected target and withholds failed metadata", () => {
    const [atlas, shop, , notes, analytics] = catalogFixture();
    expect(atlas && shop && notes && analytics).toBeTruthy();
    const shopItems = completionPlan({ prefix: "prod", connection: shop!, database: "shop_staging" }).items;
    expect(shopItems.some((item) => item.label === "products" && item.kind === "table")).toBe(true);
    const noteItems = completionPlan({ prefix: "prod", connection: notes!, database: "main" }).items;
    expect(noteItems.some((item) => item.label === "products")).toBe(false);

    const partial = structuredClone(atlas!);
    const restricted = partial.databases[0]!.schemas.find((schema) => schema.name === "restricted");
    restricted!.objects = [{ name: "secret_table", kind: "table", estimatedRows: 1, stale: false }];
    const hidden = completionPlan({ prefix: "sec", connection: partial, database: "atlas_dev" });
    expect(hidden.keywordsOnly).toBe(false);
    expect(hidden.note).toContain("restricted");
    expect(hidden.items.some((item) => item.label.includes("secret_table"))).toBe(false);
    const visible = completionPlan({ prefix: "customers", connection: partial, database: "atlas_dev" });
    expect(visible.items.some((item) => item.kind === "table" && item.label.includes("customers"))).toBe(true);
    expect(visible.items.some((item) => item.kind === "view" || item.kind === "keyword" || item.kind === "table")).toBe(true);
    expect(visible.items.every((item) => item.kind !== "table" || !item.label.includes("secret"))).toBe(true);

    const failed = structuredClone(atlas!);
    failed.databases[0]!.error = "permission denied";
    const keywords = completionPlan({ prefix: "cus", connection: failed, database: "atlas_dev" });
    expect(keywords.keywordsOnly).toBe(true);
    expect(keywords.note).toContain("permission denied");
    expect(keywords.note).toContain("只提供关键字");
    expect(keywords.items.every((item) => item.kind === "keyword")).toBe(true);

    const offline = completionPlan({ prefix: "eve", connection: analytics!, database: "analytics" });
    expect(offline.keywordsOnly).toBe(true);
    expect(offline.note).toContain("只提供关键字");
    expect(offline.items.some((item) => item.label === "events")).toBe(false);
    expect(offline.note).toContain("字段不可用");
    const withColumns = completionPlan({
      prefix: "sta",
      connection: shop!,
      database: "shop_staging",
      columns: [{ label: "status", detail: "orders" }],
    });
    expect(withColumns.items.some((item) => item.label === "status" && item.kind === "column")).toBe(true);
    const hiddenColumns = completionPlan({
      prefix: "sta",
      connection: analytics!,
      database: "analytics",
      columns: [{ label: "status", detail: "events" }],
    });
    expect(hiddenColumns.items.some((item) => item.kind === "column")).toBe(false);

    const objects = metadataNote(atlas!, "atlas_dev").objects;
    expect(qualifierFor("public.ord", objects).kind).toBe("plain");
    expect(qualifierFor("orders.", objects)).toMatchObject({ kind: "object", columnPrefix: "" });
    expect(qualifierFor("customers.", objects).kind).toBe("ambiguous");
    expect(qualifierFor("o.status", objects).kind).toBe("unknown");
  });

  it("opens an editor context menu with run, clipboard and clear actions", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    replaceCatalog(catalogFixture());
    invoke.mockImplementation(async (command: string, args?: { request?: { sql: string } }) => {
      if (command === "query_release") return true;
      return runDto({
        status: "done",
        statements: [{ sql: args?.request?.sql ?? "", status: "ok", message: "", columns: [], rows: [], truncated: false, affected: 0 }],
      });
    });
    const wrapper = mount(QueryTab, {
      global: { stubs: { teleport: true } },
      props: {
        tabId: "query-editor-menu",
        draft: "SELECT 1;\nSELECT 2;",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement);
    expect(view).toBeTruthy();

    await wrapper.get(".cm-content").trigger("contextmenu", { clientX: 40, clientY: 40 });
    const menu = wrapper.get(".menu[role='menu']");
    expect(menu.text()).toContain("执行");
    expect(menu.text()).toContain("执行脚本");
    expect(menu.text()).toContain("格式化");
    expect(menu.text()).toContain("执行计划");
    expect(menu.text()).toContain("全选");
    expect(menu.text()).toContain("清空编辑器");
    /* 无选中时复制不可用。 */
    const copyItem = menu.findAll("button").find((button) => button.text() === "复制")!;
    expect(copyItem.attributes("disabled")).toBeDefined();

    await menu.findAll("button").find((button) => button.text() === "全选")!.trigger("click");
    await nextTick();
    expect(view!.state.selection.main.from).toBe(0);
    expect(view!.state.selection.main.to).toBe(view!.state.doc.length);

    await wrapper.get(".cm-content").trigger("contextmenu", { clientX: 40, clientY: 40 });
    await wrapper.get(".menu[role='menu']").findAll("button").find((button) => button.text() === "复制")!.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenLastCalledWith("SELECT 1;\nSELECT 2;");

    await wrapper.get(".cm-content").trigger("contextmenu", { clientX: 40, clientY: 40 });
    await wrapper.get(".menu[role='menu']").findAll("button").find((button) => button.text() === "清空编辑器")!.trigger("click");
    await nextTick();
    expect(view!.state.doc.toString()).toBe("");
    wrapper.unmount();
  });

  it("runs and copies one statement from the outline context menu", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    replaceCatalog(catalogFixture());
    const calls: { command: string; sql?: string }[] = [];
    invoke.mockImplementation(async (command: string, args?: { request?: { sql: string } }) => {
      if (command === "query_release") return true;
      calls.push({ command, sql: args?.request?.sql });
      return runDto({
        status: "done",
        statements: [{ sql: args?.request?.sql ?? "", status: "ok", message: "", columns: [], rows: [], truncated: false, affected: 0 }],
      });
    });
    const wrapper = mount(QueryTab, {
      global: { stubs: { teleport: true } },
      props: {
        tabId: "query-outline-menu",
        draft: "SELECT 1;\nSELECT 2;",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    /* 多语句脚本自动展开大纲。 */
    expect(wrapper.findAll(".outline-item")).toHaveLength(2);

    await wrapper.findAll(".outline-item")[0]!.trigger("contextmenu", { clientX: 900, clientY: 200 });
    const menu = wrapper.get(".menu[role='menu']");
    expect(menu.text()).toContain("执行此语句");
    expect(menu.text()).toContain("复制语句");
    await menu.findAll("button").find((button) => button.text() === "复制语句")!.trigger("click");
    await flushPromises();
    expect(writeText).toHaveBeenLastCalledWith("SELECT 1;");

    /* 右键执行第二条：光标不动，只发送那一条。 */
    await wrapper.findAll(".outline-item")[1]!.trigger("contextmenu", { clientX: 900, clientY: 220 });
    await wrapper.get(".menu[role='menu']").findAll("button").find((button) => button.text() === "执行此语句")!.trigger("click");
    await flushPromises();
    expect(calls.filter((call) => call.command === "query_start").at(-1)).toMatchObject({ sql: "SELECT 2;" });
    expect(wrapper.get(".editor-position").text()).toContain("语句 1/2");
    wrapper.unmount();
  });

  it("hides the results pane for fullscreen sql and restores it, also on a new run", async () => {
    replaceCatalog(catalogFixture());
    invoke.mockImplementation(async (command: string, args?: { request?: { sql: string } }) => {
      if (command === "query_release") return true;
      return runDto({
        status: "done",
        statements: [{ sql: args?.request?.sql ?? "", status: "ok", message: "", columns: ["v"], rows: [["1"]], truncated: false, affected: null }],
      });
    });
    const wrapper = mount(QueryTab, {
      global: { stubs: { teleport: true } },
      props: {
        tabId: "query-toggle-results",
        draft: "SELECT 1;",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    const pane = wrapper.get("section.query-pane");
    expect(pane.attributes("data-results-hidden")).toBeUndefined();
    /* 编辑器右键菜单里也有隐藏结果入口。 */
    await wrapper.get(".cm-content").trigger("contextmenu", { clientX: 40, clientY: 40 });
    expect(wrapper.get(".menu[role='menu']").text()).toContain("隐藏结果");
    await wrapper.get(".menu[role='menu']").findAll("button").find((button) => button.text().includes("隐藏结果"))!.trigger("click");
    await nextTick();
    expect(wrapper.get("section.query-pane").attributes("data-results-hidden")).toBe("true");
    expect(wrapper.find("[data-action='show-results']").exists()).toBe(true);

    /* 隐藏状态下执行，结果区自动回来。 */
    await wrapper.get("[data-action='run']").trigger("click");
    await flushPromises();
    expect(wrapper.get("section.query-pane").attributes("data-results-hidden")).toBeUndefined();
    /* 再次隐藏后用折叠条恢复，编辑/结果比例不变。 */
    await wrapper.get("[data-action='hide-results']").trigger("click");
    await nextTick();
    expect(wrapper.get("section.query-pane").attributes("data-results-hidden")).toBe("true");
    await wrapper.get("[data-action='show-results']").trigger("click");
    await nextTick();
    expect(wrapper.get("section.query-pane").attributes("data-results-hidden")).toBeUndefined();
    wrapper.unmount();
  });

  it("shows the connection and database, and sends only the chosen sql", async () => {
    replaceCatalog(catalogFixture());
    const calls: { command: string; sql?: string }[] = [];
    invoke.mockImplementation(async (command: string, args?: { request?: { sql: string } }) => {
      if (command === "query_release") return true;
      calls.push({ command, sql: args?.request?.sql });
      return runDto({
        status: "done",
        statements: [{ sql: args?.request?.sql ?? "", status: "ok", message: "", columns: [], rows: [], truncated: false, affected: 0 }],
      });
    });
    const sql = "SELECT 1;\nSELECT 2;";
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-1",
        draft: sql,
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    expect(wrapper.text()).not.toContain("查询的答案，会出现在这里。");
    expect(wrapper.text()).not.toContain("执行全部");
    expect(wrapper.findAll(".q-guide")).toHaveLength(1);
    /* 常态下状态条保持安静；选中 SQL 时才出现提示。 */
    expect(wrapper.get(".q-guide").text()).toBe("");
    expect(wrapper.get(".editor-position").text()).toContain("语句 1/2");
    expect(wrapper.get(".editor-position").text()).toContain("Ln ");
    expect(wrapper.text()).not.toContain("SQL 编辑器");
    expect(wrapper.text()).not.toContain("选择执行目标，编写 SQL，然后点击执行");
    expect(wrapper.text()).not.toContain("Ctrl+Enter 执行光标所在语句");
    expect(wrapper.find(".cm-placeholder").exists()).toBe(false);
    expect(wrapper.findAll("button.primary").map((button) => button.attributes("data-action"))).toEqual(["run"]);
    expect(wrapper.get("[data-action='run-menu']").classes()).not.toContain("primary");
    expect(wrapper.find("[data-action='cancel']").exists()).toBe(false);
    expect(wrapper.get("[aria-label='查询']").attributes("data-tab-id")).toBe("query-1");
    expect(wrapper.get("[data-action='target']").attributes("aria-label")).toContain("执行目标");
    expect(wrapper.get("[data-action='target']").text()).toContain("Shop / shop_staging");
    expect(wrapper.get("[data-action='target']").text()).toContain("MY");
    expect(wrapper.text()).toContain("已连接");
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement);
    expect(view).toBeTruthy();
    const from = sql.indexOf("SELECT 2;");
    view!.dispatch({ selection: { anchor: from, head: from + "SELECT 2;".length } });
    await nextTick();
    /* 选中 SQL 时状态条出现「执行选中」提示。 */
    expect(wrapper.get(".q-guide").text()).toContain("执行选中");
    await wrapper.get("[data-action='run']").trigger("click");
    await flushPromises();
    const started = () => calls.filter((call) => call.command === "query_start");
    expect(started().at(-1)).toMatchObject({ command: "query_start", sql: "SELECT 2;" });
    expect(wrapper.find(".q-hint").exists()).toBe(false);

    view!.dispatch({ selection: { anchor: 0, head: 0 } });
    await nextTick();
    await wrapper.get("[data-action='run-menu']").trigger("click");
    await wrapper.get("[data-action='run-current']").trigger("click");
    await flushPromises();
    expect(started().at(-1)).toMatchObject({ command: "query_start", sql: "SELECT 1;" });

    view!.dispatch({ selection: { anchor: from + 2, head: from + 2 } });
    await nextTick();
    expect(wrapper.get(".editor-position").text()).toContain("语句 2/2");
    await wrapper.get("[data-action='run']").trigger("click");
    await flushPromises();
    expect(started().at(-1)).toMatchObject({ command: "query_start", sql: "SELECT 2;" });
    wrapper.unmount();
  });

  it("shows one operation line until a field note or a failure reason", async () => {
    const idle = mount(QueryTab, {
      props: {
        tabId: "query-idle",
        draft: "SELECT 1",
        connectionId: null,
        connectionName: null,
        database: null,
        engine: null,
        connectionStatus: null,
        notice: null,
      },
    });
    await nextTick();
    expect(idle.findAll(".q-guide")).toHaveLength(1);
    expect(idle.find(".q-hint").exists()).toBe(false);
    expect(idle.text()).toContain("等待执行");
    expect(idle.text()).toContain("Ln ");
    expect(idle.text()).not.toContain("补全只提供关键字");
    expect(idle.findAll("button.primary").map((button) => button.attributes("data-action"))).toEqual(["run"]);
    idle.unmount();

    replaceCatalog(catalogFixture());
    const steady = [
      { tabId: "query-offline", connectionId: "analytics", connectionName: "Analytics", database: "analytics", engine: "mariadb", connectionStatus: "disconnected" as const },
      { tabId: "query-unread", connectionId: "shop", connectionName: "Shop", database: "missing_db", engine: "mysql", connectionStatus: "connected" as const },
      { tabId: "query-gone", connectionId: "missing", connectionName: "Old", database: "db", engine: "mysql", connectionStatus: "connected" as const },
    ];
    for (const props of steady) {
      const wrapper = mount(QueryTab, { props: { ...props, draft: "SELECT 1", notice: null } });
      await nextTick();
      expect(wrapper.find(".q-hint").exists()).toBe(false);
      expect(wrapper.findAll(".q-guide")).toHaveLength(1);
      expect(wrapper.text()).not.toContain("补全只提供关键字");
      wrapper.unmount();
    }

    const bareCatalog = catalogFixture();
    bareCatalog[1]!.databases[0]!.schemas[0]!.objects = [];
    bareCatalog[1]!.databases[0]!.schemas[0]!.total = 0;
    replaceCatalog(bareCatalog);
    const bare = mount(QueryTab, {
      props: {
        tabId: "query-bare",
        draft: "SELECT 1",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    expect(bare.find(".q-hint").exists()).toBe(false);
    bare.unmount();

    const failedCatalog = catalogFixture();
    failedCatalog[1]!.databases[0]!.error = "permission denied";
    replaceCatalog(failedCatalog);
    const failed = mount(QueryTab, {
      props: {
        tabId: "query-meta-failed",
        draft: "SELECT 1",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    expect(failed.get(".q-hint").text()).toContain("失败");
    expect(failed.get(".q-hint").text()).toContain("permission denied");
    expect(failed.findAll(".q-guide")).toHaveLength(1);
    failed.unmount();

    replaceCatalog(catalogFixture());
    const partial = mount(QueryTab, {
      props: {
        tabId: "query-partial",
        draft: "SELECT 1",
        connectionId: "atlas",
        connectionName: "Atlas",
        database: "atlas_dev",
        engine: "postgres",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    expect(partial.get(".q-hint").text()).toContain("部分对象读取失败");
    partial.unmount();
  });

  it("marks each execution target with its engine when names match", async () => {
    const rows = catalogFixture();
    rows[0].name = "127.0.0.1";
    rows[0].databases = [
      { ...rows[0].databases[0], name: "postgres" },
      { ...rows[0].databases[1], name: "rillsql" },
    ];
    rows[1].name = "127.0.0.1";
    rows[1].databases[0].name = "rillsql";
    replaceCatalog(rows);
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-target",
        draft: "",
        connectionId: "shop",
        connectionName: "127.0.0.1",
        database: "rillsql",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    /* 空草稿 + 已选目标：编辑器内出现占位提示，状态条保持安静。 */
    expect(wrapper.find(".cm-placeholder").exists()).toBe(true);
    expect(wrapper.get(".q-guide").text()).toBe("");
    expect(wrapper.text()).not.toContain("执行全部");
    expect(wrapper.text()).not.toContain("查询的答案，会出现在这里。");
    await wrapper.get("[data-action='target']").trigger("click");
    const choices = wrapper.findAll("[role='option']");
    const rillsql = choices.filter((item) => item.text().includes("127.0.0.1 / rillsql"));
    expect(rillsql.map((item) => item.attributes("data-engine"))).toEqual(["postgres", "mysql"]);
    expect(rillsql[0].text()).toContain("PostgreSQL");
    expect(rillsql[1].text()).toContain("MySQL");
    expect(rillsql[1].attributes("aria-selected")).toBe("true");
    expect(wrapper.find("select").exists()).toBe(false);
    await rillsql[0].trigger("click");
    expect(wrapper.emitted("target")?.[0]?.[0]).toMatchObject({ connectionId: "atlas", database: "rillsql", engine: "postgres" });
    wrapper.unmount();
  });

  it("adopts an editor that is still on the page when the closure lost it", () => {
    const host = document.createElement("div");
    document.body.appendChild(host);
    const live = new EditorView({ parent: host, state: EditorState.create({ doc: "select 1" }) });
    const resolved = resolveEditor(undefined, host, () => {
      throw new Error("不应新建");
    });
    expect(resolved).toBe(live);
    expect(resolved?.state.doc.toString()).toBe("select 1");
    live.destroy();
    host.remove();
  });

  it("rebuilds an editor that is no longer in the page", () => {
    const host = document.createElement("div");
    document.body.appendChild(host);
    const gone = new EditorView({ parent: host, state: EditorState.create({ doc: "old" }) });
    gone.destroy();
    const created = resolveEditor(gone, host, (parent) => new EditorView({
      parent,
      state: EditorState.create({ doc: "select * from customers" }),
    }));
    expect(created?.dom.isConnected).toBe(true);
    expect(created?.state.doc.toString()).toBe("select * from customers");
    expect(resolveEditor(undefined, null, () => {
      throw new Error("没有容器");
    })).toBeUndefined();
    created?.destroy();
    host.remove();
  });

  it("shows that a run has started before the database answers", async () => {
    replaceCatalog(catalogFixture());
    let finish: () => void = () => undefined;
    invoke.mockImplementation((command: string, args?: { request?: { sql: string } }) => new Promise((resolve) => {
      if (command !== "query_start") {
        resolve({});
        return;
      }
      finish = () => resolve(runDto({
        status: "done",
        statements: [{ sql: args?.request?.sql ?? "", status: "ok", message: "", columns: ["id"], rows: [["1"]], truncated: false, affected: null }],
      }));
    }));
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-pending",
        draft: "select * from customers",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    const pending = wrapper.get("[data-action='run']").trigger("click");
    await nextTick();
    await flushPromises();
    expect(wrapper.get("[data-action='run']").text()).toContain("正在执行…");
    expect(wrapper.get("[data-action='run']").attributes("disabled")).toBeDefined();
    finish();
    await pending;
    await flushPromises();
    expect(wrapper.get("[data-action='run']").text()).toContain("执行");
    wrapper.unmount();
  });

  it("still runs the draft after the editor view has been destroyed", async () => {
    replaceCatalog(catalogFixture());
    const calls: { command: string; sql?: string }[] = [];
    invoke.mockImplementation(async (command: string, args?: { request?: { sql: string } }) => {
      calls.push({ command, sql: args?.request?.sql });
      return runDto({
        status: "done",
        statements: [{ sql: args?.request?.sql ?? "", status: "ok", message: "", columns: ["id"], rows: [["1"]], truncated: false, affected: null }],
      });
    });
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-rebuild",
        draft: "select * from customers",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)?.destroy();
    await wrapper.get("[data-action='run']").trigger("click");
    await flushPromises();
    expect(calls.filter((call) => call.command === "query_start").at(-1)?.sql).toBe("select * from customers");
    expect(wrapper.text()).toContain("1");
    expect(wrapper.text()).not.toContain("执行 SQL 后，结果显示在这里");
    wrapper.unmount();
  });

  it("shows the database error instead of staying idle", async () => {
    replaceCatalog(catalogFixture());
    invoke.mockRejectedValue("连接未打开，没有另开数据库连接。");
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-error",
        draft: "select * from customers",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    await wrapper.get("[data-action='run']").trigger("click");
    await flushPromises();
    expect(wrapper.get("[role='alert']").text()).toContain("连接未打开，没有另开数据库连接。");
    expect(wrapper.get("[data-action='run']").text()).toContain("执行");
    wrapper.unmount();
  });

  it("uses the existing execute path when the command palette asks to run", async () => {
    replaceCatalog(catalogFixture());
    const calls: string[] = [];
    invoke.mockImplementation(async (command: string, args?: { request?: { sql: string } }) => {
      if (command === "query_start") calls.push(args?.request?.sql ?? "");
      return runDto({
        status: "done",
        statements: [{ sql: args?.request?.sql ?? "", status: "ok", message: "", columns: [], rows: [], truncated: false, affected: 0 }],
      });
    });
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-command",
        draft: "SELECT 1;\nSELECT 2;",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
        executeRequest: 0,
      },
    });
    await nextTick();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    const from = "SELECT 1;\n".length;
    view.dispatch({ selection: { anchor: from, head: from + "SELECT 2;".length } });
    await nextTick();
    await wrapper.setProps({ executeRequest: 1 });
    await flushPromises();
    expect(calls).toEqual(["SELECT 2;"]);
    expect(wrapper.find("[data-action='run']").exists()).toBe(true);
    wrapper.unmount();
  });

  it("refuses to re-run while result edits are pending", async () => {
    replaceCatalog(catalogFixture());
    const calls: string[] = [];
    invoke.mockImplementation(async (command: string, args?: { request?: { sql: string } }) => {
      if (command === "query_start") calls.push(args?.request?.sql ?? "");
      return runDto({ status: "done", statements: [] });
    });
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-pending-result",
        draft: "SELECT 1;",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
        executeRequest: 0,
      },
    });
    await flushPromises();
    const stop = registerPending("query-pending-result", () => true);
    await wrapper.setProps({ executeRequest: 1 });
    await flushPromises();
    expect(calls).toEqual([]);
    expect(wrapper.get("[role='alert']").text()).toContain("未提交的更改");
    stop();
    wrapper.unmount();
  });

  it("keeps finished rows when a later statement fails, and marks an unknown write", async () => {
    adoptRun("query-9", runDto({
      status: "error",
      statements: [
        { sql: "SELECT 1", status: "ok", message: "", columns: ["n"], rows: [["7"]], truncated: false, affected: null },
        { sql: "SELECT bad", status: "error", message: "syntax error", columns: [], rows: [], truncated: false, affected: null },
        { sql: "SELECT 3", status: "skipped", message: "", columns: [], rows: [], truncated: false, affected: null },
      ],
    }));
    const failed = mount(QueryTab, {
      props: {
        tabId: "query-9",
        draft: "",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    expect(failed.text()).toContain("7");
    expect(failed.text()).toContain("第 2 条语句失败");
    expect(failed.text()).toContain("已完成 1 条的结果保留");
    expect(failed.text()).toContain("其余 1 条未执行");
    expect(failed.text()).toContain("syntax error");
    expect(failed.text()).toContain("在编辑器中定位");
    expect(failed.get("[aria-label='查询结果']").text()).toContain("7");
    failed.unmount();

    adoptRun("query-8", runDto({
      id: "run-unknown",
      status: "unknown",
      statements: [{ sql: "UPDATE t SET n = 1", status: "unknown", message: "连接中断", columns: [], rows: [], truncated: false, affected: null }],
    }));
    const unknown = mount(QueryTab, {
      props: {
        tabId: "query-8",
        draft: "UPDATE t SET n = 1",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "lost",
        notice: null,
      },
    });
    expect(unknown.text()).toContain("写入结果未知");
    expect(unknown.text()).toContain("不会自动重放");
    expect(unknown.text()).toContain("连接已断开");
    unknown.unmount();
  });

  it("distinguishes requested, confirmed and failed cancel, and closes only after confirmation", async () => {
    adoptRun("query-3", runDto({ status: "running", cancel: "none", viaSsh: true, statements: [{ sql: "SELECT 1", status: "running", message: "", columns: [], rows: [], truncated: false, affected: null }] }));
    invoke.mockResolvedValue(runDto({ status: "cancel-requested", cancel: "requested", viaSsh: true, freshChannel: true, cancelMessage: "已另开一条通道" }));
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-3",
        draft: "SELECT 1",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    expect(wrapper.find("[data-action='run']").exists()).toBe(false);
    expect(wrapper.find("[data-action='run-menu']").exists()).toBe(false);
    expect(wrapper.get("[data-action='cancel']").text()).toBe("取消");
    expect(wrapper.findAll("button.primary")).toHaveLength(0);
    await wrapper.get("[data-action='cancel']").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("取消中");
    expect(wrapper.text()).not.toContain("数据库已确认取消");
    expect(wrapper.text()).not.toContain("另开一条通道");
    expect(wrapper.get("[data-action='cancel']").text()).toBe("取消中…");
    await wrapper.get(".q-details summary").trigger("click");
    expect(wrapper.text()).toContain("另开一条通道");
    wrapper.unmount();

    const closed: string[] = [];
    attachCloser((id) => closed.push(id));
    adoptRun("query-4", runDto({ id: "run-4", status: "running", cancel: "none" }));
    invoke.mockResolvedValue(runDto({ id: "run-4", status: "cancelled", cancel: "confirmed", cancelMessage: "数据库已确认取消" }));
    expect(guardClose("query-4")).toBe(false);
    await flushPromises();
    expect(closed).toEqual(["query-4"]);

    adoptRun("query-5", runDto({ id: "run-5", status: "running", cancel: "none" }));
    invoke.mockRejectedValue("取消失败：权限不足");
    expect(guardClose("query-5")).toBe(false);
    await flushPromises();
    expect(tabRun("query-5")?.cancel).toBe("failed");
    expect(tabRun("query-5")?.status).toBe("running");
    expect(closed).toEqual(["query-4"]);
    await requestCancel("query-5");
    expect(tabRun("query-5")?.cancelMessage).toContain("权限不足");
  });

  it("lets another tab stay editable while a connection is busy, and drops drafts on a new session", async () => {
    const session = createSession();
    session.openQuery();
    session.openQuery();
    const [first, second] = session.tabs.value;
    adoptRun(first!.id, runDto({ status: "running", connectionId: "shop" }));
    const before = Object.keys(localStorage);
    session.setDraft(second!.id, "SELECT other_tab");
    expect(session.tabs.value[1]?.draft).toBe("SELECT other_tab");
    expect(connectionBusy("shop", second!.id)).toBe(true);
    await expect(startTabRun({
      tabId: second!.id,
      connectionId: "shop",
      connectionName: "Shop",
      database: "shop_staging",
      engine: "mysql",
      sql: "SELECT other_tab",
      limit: 100,
    })).rejects.toThrow(/其他标签/);
    expect(Object.keys(localStorage)).toEqual(before);
    const restarted = createSession();
    expect(restarted.tabs.value).toEqual([]);
  });

  it("completes a described column after orders and shows why fields are unavailable", async () => {
    replaceCatalog(catalogFixture());
    const described: { table?: string; schema?: string }[] = [];
    invoke.mockImplementation(async (command: string, args?: { request?: { table?: string; schema?: string } }) => {
      if (command !== "grid_describe") throw new Error(command);
      described.push({ table: args?.request?.table, schema: args?.request?.schema });
      return {
        engine: "mysql",
        schema: "",
        name: "orders",
        kind: "table",
        key: ["id"],
        columns: [{ name: "status", typeName: "varchar", nullable: false, primary: false, unique: false, auto: false }],
      };
    });
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-complete",
        draft: "SELECT orders.",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement);
    expect(view).toBeTruthy();
    view!.dispatch({ selection: { anchor: view!.state.doc.length } });
    startCompletion(view!);
    await vi.waitFor(() => {
      expect(currentCompletions(view!.state).some((item) => item.label === "status")).toBe(true);
    });
    expect(described).toEqual([{ table: "orders", schema: "" }]);
    expect(currentCompletions(view!.state).some((item) => item.label === "secret_status")).toBe(false);

    view!.dispatch({ changes: { from: 0, to: view!.state.doc.length, insert: "sta" }, selection: { anchor: 3 } });
    startCompletion(view!);
    await vi.waitFor(() => {
      expect(currentCompletions(view!.state).some((item) => item.label === "status")).toBe(true);
    });
    expect(described).toHaveLength(1);
    wrapper.unmount();
    clearColumnCache();

    invoke.mockImplementation(async (command: string) => {
      if (command === "grid_describe") throw "permission denied";
      throw new Error(command);
    });
    const failed = mount(QueryTab, {
      props: {
        tabId: "query-complete-failed",
        draft: "SELECT orders.",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    const failedView = EditorView.findFromDOM(failed.get(".cm-content").element as HTMLElement)!;
    failedView.dispatch({ selection: { anchor: failedView.state.doc.length } });
    startCompletion(failedView);
    await vi.waitFor(() => {
      expect(failed.get(".q-hint").text()).toContain("permission denied");
    });
    expect(failed.get(".q-hint").text()).toContain("不会猜测字段名");
    expect(currentCompletions(failedView.state).some((item) => item.label === "status")).toBe(false);
    failed.unmount();

    const missing = mount(QueryTab, {
      props: {
        tabId: "query-complete-none",
        draft: "SELECT orders.",
        connectionId: null,
        connectionName: null,
        database: null,
        engine: null,
        connectionStatus: null,
        notice: null,
      },
    });
    await nextTick();
    expect(missing.find(".q-hint").exists()).toBe(false);
    expect(missing.findAll(".q-guide")).toHaveLength(1);
    invoke.mockClear();
    const missingView = EditorView.findFromDOM(missing.get(".cm-content").element as HTMLElement)!;
    missingView.dispatch({ selection: { anchor: missingView.state.doc.length } });
    startCompletion(missingView);
    await vi.waitFor(() => {
      expect(missing.get(".q-hint").text()).toContain("字段不可用");
    });
    await flushPromises();
    expect(invoke.mock.calls.some((call) => call[0] === "grid_describe")).toBe(false);
    expect(currentCompletions(missingView.state).some((item) => item.label === "status")).toBe(false);
    missing.unmount();
  });

  it("formats the selection in the editor and refuses an unclosed quote", async () => {
    const sql = "select  1;\nselect   2;";
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-format",
        draft: sql,
        connectionId: null,
        connectionName: null,
        database: null,
        engine: null,
        connectionStatus: null,
        notice: null,
      },
    });
    await nextTick();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    const from = sql.indexOf("select   2;");
    view.dispatch({ selection: { anchor: from, head: sql.length } });
    await wrapper.get("[data-action='format']").trigger("click");
    const next = view.state.doc.toString();
    expect(next.slice(0, from)).toBe(sql.slice(0, from));
    expect(next.slice(from)).toBe("SELECT\n  2;");
    wrapper.unmount();

    const broken = mount(QueryTab, {
      props: {
        tabId: "query-format-broken",
        draft: "select 'abc",
        connectionId: null,
        connectionName: null,
        database: null,
        engine: null,
        connectionStatus: null,
        notice: null,
      },
    });
    await nextTick();
    await broken.get("[data-action='format']").trigger("click");
    const brokenView = EditorView.findFromDOM(broken.get(".cm-content").element as HTMLElement)!;
    expect(brokenView.state.doc.toString()).toBe("select 'abc");
    expect(broken.get("[role='alert']").text()).toContain("引号");
    broken.unmount();
  });

  it("keeps the editor text when formatting would break a comment or glue 1from t", async () => {
    const commented = "select 1 -- select 2 from t";
    const commentTab = mount(QueryTab, {
      props: {
        tabId: "query-format-comment",
        draft: commented,
        connectionId: null,
        connectionName: null,
        database: null,
        engine: null,
        connectionStatus: null,
        notice: null,
      },
    });
    await nextTick();
    const commentView = EditorView.findFromDOM(commentTab.get(".cm-content").element as HTMLElement)!;
    const commentFrom = commented.indexOf("select 2");
    commentView.dispatch({ selection: { anchor: commentFrom, head: commented.length } });
    await commentTab.get("[data-action='format']").trigger("click");
    expect(commentView.state.doc.toString()).toBe(commented);
    expect(commentTab.get("[role='alert']").text()).toContain("行注释");
    commentTab.unmount();

    const glued = "select 1 from t";
    const glueTab = mount(QueryTab, {
      props: {
        tabId: "query-format-glue",
        draft: glued,
        connectionId: null,
        connectionName: null,
        database: null,
        engine: null,
        connectionStatus: null,
        notice: null,
      },
    });
    await nextTick();
    const glueView = EditorView.findFromDOM(glueTab.get(".cm-content").element as HTMLElement)!;
    const glueFrom = glued.indexOf("1 ");
    glueView.dispatch({ selection: { anchor: glueFrom, head: glueFrom + "1 ".length } });
    await glueTab.get("[data-action='format']").trigger("click");
    expect(glueView.state.doc.toString()).toBe(glued);
    expect(glueTab.get("[role='alert']").text()).toContain("空白");
    glueTab.unmount();
  });

  it("requests one EXPLAIN plan and does not run empty or multiple statements", async () => {
    replaceCatalog(catalogFixture());
    const calls: { command: string; sql?: string }[] = [];
    invoke.mockImplementation(async (command: string, args?: { request?: { sql: string } }) => {
      if (command === "query_release") return true;
      calls.push({ command, sql: args?.request?.sql });
      return runDto({
        status: "done",
        statements: [{
          sql: args?.request?.sql ?? "",
          status: "ok",
          message: "",
          columns: ["QUERY PLAN"],
          rows: [["Seq Scan on orders"]],
          truncated: false,
          affected: null,
        }],
      });
    });
    const sql = "SELECT * FROM orders;\nSELECT 2;";
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-plan",
        draft: sql,
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    await wrapper.get("[data-action='plan']").trigger("click");
    await flushPromises();
    expect(calls.map((call) => call.command)).toEqual(["query_start"]);
    expect(calls[0]?.sql).toBe("EXPLAIN FORMAT=TREE SELECT * FROM orders");
    expect(calls[0]?.sql?.toLowerCase()).not.toContain("analyze");
    expect(wrapper.get("[data-result='plan']").text()).toContain("Seq Scan on orders");
    expect(wrapper.get("[data-result='plan']").text()).toContain("未使用 ANALYZE");
    expect(wrapper.find("[aria-label='查询结果']").exists()).toBe(false);

    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    view.dispatch({ selection: { anchor: 0, head: sql.length } });
    await wrapper.get("[data-action='plan']").trigger("click");
    await flushPromises();
    expect(calls).toHaveLength(1);
    expect(wrapper.get("[role='alert']").text()).toContain("一条");
    wrapper.unmount();

    const sqliteCalls: string[] = [];
    invoke.mockImplementation(async (command: string, args?: { request?: { sql: string } }) => {
      if (command === "query_release") return true;
      sqliteCalls.push(args?.request?.sql ?? command);
      return runDto({
        engine: "sqlite",
        database: "main",
        statements: [{ sql: args?.request?.sql ?? "", status: "ok", message: "", columns: ["detail"], rows: [["SCAN notes"]], truncated: false, affected: null }],
      });
    });
    const sqlite = mount(QueryTab, {
      props: {
        tabId: "query-plan-sqlite",
        draft: "SELECT * FROM notes",
        connectionId: "notes",
        connectionName: "本地笔记",
        database: "main",
        engine: "sqlite",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    await sqlite.get("[data-action='plan']").trigger("click");
    await flushPromises();
    expect(sqliteCalls).toEqual(["EXPLAIN QUERY PLAN SELECT * FROM notes"]);
    sqlite.unmount();

    invoke.mockClear();
    const offline = mount(QueryTab, {
      props: {
        tabId: "query-plan-offline",
        draft: "SELECT * FROM events",
        connectionId: "analytics",
        connectionName: "Analytics",
        database: "analytics",
        engine: "mariadb",
        connectionStatus: "disconnected",
        notice: null,
      },
    });
    await nextTick();
    await offline.get("[data-action='plan']").trigger("click");
    await flushPromises();
    expect(invoke.mock.calls).toHaveLength(0);
    expect(offline.get("[role='alert']").text()).toContain("未连接");
    offline.unmount();

    const none = mount(QueryTab, {
      props: {
        tabId: "query-plan-none",
        draft: "",
        connectionId: null,
        connectionName: null,
        database: null,
        engine: null,
        connectionStatus: null,
        notice: null,
      },
    });
    await nextTick();
    await none.get("[data-action='plan']").trigger("click");
    expect(none.get("[role='alert']").text()).toContain("请选择执行目标");
    none.unmount();
  });

  it("does not start a run when ANALYZE remains after strings, identifiers and comments", async () => {
    replaceCatalog(catalogFixture());
    const calls: string[] = [];
    invoke.mockImplementation(async (command: string) => {
      calls.push(command);
      throw new Error("should not run");
    });
    for (const [index, draft] of [
      "EXPLAIN(ANALYZE) SELECT 1",
      "EXPLAIN (VERBOSE, ANALYZE) SELECT 1",
      "EXPLAIN /*c*/ ANALYZE SELECT 1",
      "SELECT 1 /*! ANALYZE */",
      "SELECT 1 /*!50000 ANALYSE */",
    ].entries()) {
      const wrapper = mount(QueryTab, {
        props: {
          tabId: `query-plan-analyze-${index}`,
          draft,
          connectionId: "atlas",
          connectionName: "Atlas",
          database: "atlas_dev",
          engine: "postgres",
          connectionStatus: "connected",
          notice: null,
        },
      });
      await nextTick();
      await wrapper.get("[data-action='plan']").trigger("click");
      await flushPromises();
      expect(calls).toEqual([]);
      expect(wrapper.get("[role='alert']").text()).toBe("计划不使用 ANALYZE，没有发送这条语句。");
      wrapper.unmount();
    }
  });

  it("sends the whole script only from 执行脚本 and keeps the main button on the selection", async () => {
    replaceCatalog(catalogFixture());
    const calls: string[] = [];
    invoke.mockImplementation(async (_command: string, args?: { request?: { sql: string } }) => {
      if (_command === "query_release") return true;
      calls.push(args?.request?.sql ?? "");
      return runDto({ status: "done", statements: [{ sql: args?.request?.sql ?? "", status: "ok", message: "", columns: [], rows: [], truncated: false, affected: 1 }] });
    });
    const sql = "SELECT 1;\nSELECT 2;";
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-script",
        draft: sql,
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    const editor = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    editor.dispatch({ selection: { anchor: 0, head: "SELECT 1;".length } });
    await nextTick();
    expect(wrapper.get("[data-action='run']").text()).toContain("执行选中");
    expect(wrapper.text()).not.toContain("执行全部");
    await wrapper.get("[data-action='run']").trigger("click");
    await flushPromises();
    expect(calls).toEqual(["SELECT 1;"]);
    expect(wrapper.html()).toContain("cm-send-range");
    await wrapper.get("[data-action='run-menu']").trigger("click");
    await wrapper.get("[data-action='run-script']").trigger("click");
    await flushPromises();
    expect(calls[1]).toBe(sql);
    wrapper.unmount();
  });

  it("changes the selection while dragging and does not execute", async () => {
    replaceCatalog(catalogFixture());
    invoke.mockResolvedValue(runDto({ status: "done" }));
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-drag",
        draft: "SELECT 1;\nSELECT 2;",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    const editor = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    editor.dispatch({ selection: { anchor: 0, head: 8 } });
    await wrapper.get(".cm-content").trigger("pointerdown");
    await wrapper.get(".cm-content").trigger("pointermove");
    await flushPromises();
    expect(invoke).not.toHaveBeenCalled();
    expect(wrapper.get("[data-action='run']").text()).toContain("执行选中");
    wrapper.unmount();
  });

  it("asks before analyze, sends nothing on cancel, and refuses SQLite after confirm", async () => {
    replaceCatalog(catalogFixture());
    const calls: string[] = [];
    invoke.mockImplementation(async (_command: string, args?: { request?: { sql: string } }) => {
      if (_command === "query_release") return true;
      calls.push(args?.request?.sql ?? "");
      return runDto({
        status: "done",
        engine: "postgres",
        statements: [{ sql: args?.request?.sql ?? "", status: "ok", message: "", columns: ["QUERY PLAN"], rows: [["Seq Scan"]], truncated: false, affected: null }],
      });
    });
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-analyze",
        draft: "SELECT 1",
        connectionId: "atlas",
        connectionName: "Atlas",
        database: "atlas_dev",
        engine: "postgres",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    await wrapper.get("[data-action='analyze']").trigger("click");
    expect(wrapper.text()).toContain("这条语句会在服务器上执行。");
    expect(invoke).not.toHaveBeenCalled();
    await wrapper.get("[data-action='analyze-cancel']").trigger("click");
    await flushPromises();
    expect(invoke).not.toHaveBeenCalled();
    await wrapper.get("[data-action='analyze']").trigger("click");
    await wrapper.get("[data-action='analyze-confirm']").trigger("click");
    await flushPromises();
    expect(calls).toEqual(["EXPLAIN ANALYZE SELECT 1"]);
    expect(wrapper.get("[data-result='plan']").text()).toContain("该语句已在服务器上执行");
    expect(wrapper.get("[data-result='plan']").text()).not.toContain("未使用 ANALYZE");
    wrapper.unmount();

    invoke.mockClear();
    const sqlite = mount(QueryTab, {
      props: {
        tabId: "query-analyze-sqlite",
        draft: "SELECT 1",
        connectionId: "notes",
        connectionName: "本地笔记",
        database: "main",
        engine: "sqlite",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    await sqlite.get("[data-action='analyze']").trigger("click");
    await sqlite.get("[data-action='analyze-confirm']").trigger("click");
    await flushPromises();
    expect(invoke).not.toHaveBeenCalled();
    expect(sqlite.get("[role='alert']").text()).toContain("不支持");
    sqlite.unmount();
  });

  it("keeps a procedure body together and refuses an unclosed quote", async () => {
    const sql = "DELIMITER $$\nCREATE PROCEDURE p()\nBEGIN\n  UPDATE t SET n = 1;\nEND$$\nDELIMITER ;\nSELECT 1;";
    const parts = splitSql(sql, "mysql");
    expect(parts.map((part) => part.text)).toEqual(["CREATE PROCEDURE p()\nBEGIN\n  UPDATE t SET n = 1;\nEND", "SELECT 1;"]);
    expect(parts.some((part) => part.text.includes("DELIMITER"))).toBe(false);
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-delimiter",
        draft: "SELECT 'open",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    replaceCatalog(catalogFixture());
    invoke.mockResolvedValue(runDto());
    await nextTick();
    await wrapper.get("[data-action='run']").trigger("click");
    await flushPromises();
    expect(invoke).not.toHaveBeenCalled();
    expect(wrapper.get("[role='alert']").text()).toContain("引号没有闭合");
    wrapper.unmount();
  });

  it("keeps an unquoted DEFINER routine in one statement", () => {
    const sql = "CREATE DEFINER=root PROCEDURE p()\nBEGIN\n  UPDATE t SET n = 1;\n  UPDATE t SET n = 2;\nEND";
    const parts = splitSql(sql, "mysql");
    expect(parts).toHaveLength(1);
    expect(parts[0]?.text).toContain("UPDATE t SET n = 2;");
    expect(parts[0]?.text.startsWith("CREATE DEFINER=root PROCEDURE")).toBe(true);
    const host = "CREATE DEFINER=root@localhost FUNCTION f() BEGIN SELECT 1; SELECT 2; END";
    expect(splitSql(host, "mariadb")).toHaveLength(1);
    const open = inspectSql("CREATE DEFINER=root PROCEDURE p() BEGIN UPDATE t SET n = 1;", "mysql");
    expect(open.refusal).toContain("例程体未闭合");
    expect(open.pieces).toHaveLength(0);
    const quoted = "CREATE DEFINER=`root`@`localhost` PROCEDURE p() BEGIN SELECT 1; END";
    expect(splitSql(quoted, "mysql")).toHaveLength(1);
  });

  it("does not replace an unclosed tail with the previous statement", async () => {
    replaceCatalog(catalogFixture());
    const calls: string[] = [];
    invoke.mockImplementation(async (_command: string, args?: { request?: { sql: string } }) => {
      if (_command === "query_release") return true;
      calls.push(args?.request?.sql ?? "");
      return runDto();
    });
    const draft = "DELETE FROM t;\nSELECT 'open";
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-unclosed-tail",
        draft,
        connectionId: "atlas",
        connectionName: "Atlas",
        database: "atlas_dev",
        engine: "postgres",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    const editor = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    editor.dispatch({ selection: { anchor: 0, head: "DELETE FROM t;".length } });
    await nextTick();
    await wrapper.get("[data-action='run']").trigger("click");
    await flushPromises();
    expect(calls).toEqual(["DELETE FROM t;"]);
    editor.dispatch({ selection: { anchor: draft.length } });
    await nextTick();
    await wrapper.get("[data-action='run-menu']").trigger("click");
    await wrapper.get("[data-action='run-current']").trigger("click");
    await flushPromises();
    expect(calls).toEqual(["DELETE FROM t;"]);
    expect(wrapper.get("[role='alert']").text()).toContain("引号没有闭合");
    await wrapper.get("[data-action='plan']").trigger("click");
    await flushPromises();
    expect(calls).toEqual(["DELETE FROM t;"]);
    expect(wrapper.get("[role='alert']").text()).toContain("引号没有闭合");
    wrapper.unmount();
  });

  it("marks the cursor statement when the same text appears twice", async () => {
    replaceCatalog(catalogFixture());
    invoke.mockImplementation(async (_command: string, args?: { request?: { sql: string } }) => runDto({
      status: "done",
      statements: [{
        sql: args?.request?.sql ?? "",
        status: "ok",
        message: "",
        columns: ["QUERY PLAN"],
        rows: [["Seq Scan"]],
        truncated: false,
        affected: null,
      }],
    }));
    const draft = "SELECT 1;\nSELECT 1;";
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-plan-range",
        draft,
        connectionId: "atlas",
        connectionName: "Atlas",
        database: "atlas_dev",
        engine: "postgres",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    const second = draft.lastIndexOf("SELECT 1;");
    const editor = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    editor.dispatch({ selection: { anchor: second + 2 } });
    await wrapper.get("[data-action='plan']").trigger("click");
    await flushPromises();
    const lines = wrapper.findAll(".cm-line");
    expect(lines[0]?.html()).not.toContain("cm-send-range");
    expect(lines[1]?.html()).toContain("cm-send-range");
    wrapper.unmount();
  });

  it("explains that PostgreSQL does not use DELIMITER when that is the only text", async () => {
    replaceCatalog(catalogFixture());
    invoke.mockResolvedValue(runDto());
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-delimiter-only",
        draft: "DELIMITER $$",
        connectionId: "notes",
        connectionName: "本地笔记",
        database: "main",
        engine: "sqlite",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    await wrapper.get("[data-action='run']").trigger("click");
    await flushPromises();
    expect(invoke).not.toHaveBeenCalled();
    expect(wrapper.get("[role='alert']").text()).toContain("不使用 DELIMITER");
    expect(wrapper.get("[role='alert']").text()).not.toContain("只有空白或注释");
    wrapper.unmount();
  });

  it("loads a history entry into the editor as a draft without running it", async () => {
    replaceCatalog(catalogFixture());
    const rows = [{
      id: "h1",
      sql: "SELECT count(*) FROM orders;",
      connectionId: "shop",
      connectionName: "Shop",
      databaseName: "shop_staging",
      engine: "mysql",
      startedAt: "2026-10-20T09:30:00Z",
      finishedAt: "2026-10-20T09:30:01Z",
      status: "ok",
      elapsedMs: 34,
      summary: "1 行",
    }];
    invoke.mockImplementation(async (command: string) => {
      if (command === "query_history") return rows;
      throw new Error(command);
    });
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-history",
        draft: "SELECT 1",
        connectionId: "atlas",
        connectionName: "Atlas",
        database: "atlas_dev",
        engine: "postgres",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("query_history");
    await wrapper.get("[data-action='history']").trigger("click");
    await flushPromises();
    expect(invoke.mock.calls.map((call) => call[0])).toContain("query_history");
    const item = wrapper.get("[data-action='open-history']");
    expect(item.text()).toContain("成功");
    expect(item.text()).toContain("Shop / shop_staging");
    expect(item.text()).toContain("耗时 34 毫秒");
    expect(item.text()).toContain("SELECT count(*) FROM orders;");
    await item.trigger("click");
    await flushPromises();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    expect(view.state.doc.toString()).toBe("SELECT count(*) FROM orders;");
    expect(wrapper.emitted("update:draft")?.at(-1)).toEqual(["query-history", "SELECT count(*) FROM orders;"]);
    expect(wrapper.emitted("target")?.[0]?.[0]).toMatchObject({ connectionId: "shop", connectionName: "Shop", database: "shop_staging", engine: "mysql", status: "connected" });
    expect(wrapper.find("[data-action='open-history']").exists()).toBe(false);
    expect(invoke.mock.calls.some((call) => call[0] === "query_start")).toBe(false);
    wrapper.unmount();
  });

  it("loads only the sql and explains when the history connection is gone", async () => {
    replaceCatalog(catalogFixture());
    const rows = [{
      id: "h2",
      sql: "DELETE FROM audit_log;",
      connectionId: "ghost",
      connectionName: "旧连接",
      databaseName: "gone_db",
      engine: "postgres",
      startedAt: "2026-10-19T08:00:00Z",
      finishedAt: "2026-10-19T08:00:02Z",
      status: "error",
      elapsedMs: 1200,
      summary: "",
    }];
    invoke.mockImplementation(async (command: string) => {
      if (command === "query_history") return rows;
      throw new Error(command);
    });
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-history-gone",
        draft: "SELECT 1",
        connectionId: "atlas",
        connectionName: "Atlas",
        database: "atlas_dev",
        engine: "postgres",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    await wrapper.get("[data-action='history']").trigger("click");
    await flushPromises();
    expect(wrapper.get("[data-action='open-history']").text()).toContain("原连接已删除");
    await wrapper.get("[data-action='open-history']").trigger("click");
    await flushPromises();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    expect(view.state.doc.toString()).toBe("DELETE FROM audit_log;");
    expect(wrapper.emitted("update:draft")?.at(-1)).toEqual(["query-history-gone", "DELETE FROM audit_log;"]);
    expect(wrapper.get("[role='alert']").text()).toContain("需要重新选择执行目标。原来的连接已删除，这次只载入 SQL，不会执行。");
    expect(wrapper.emitted("target")).toBeUndefined();
    expect(invoke.mock.calls.some((call) => call[0] === "query_start")).toBe(false);
    wrapper.unmount();
  });

  it("saves the editor sql by name and reopens a saved query as a draft", async () => {
    replaceCatalog(catalogFixture());
    const saved = [{
      id: "s1",
      name: "周报",
      sql: "SELECT * FROM orders WHERE week = 42;",
      connectionId: "atlas",
      connectionName: "Atlas",
      databaseName: "atlas_dev",
      engine: "postgres",
      updatedAt: "2026-10-18T12:00:00Z",
    }];
    const savedRequests: unknown[] = [];
    invoke.mockImplementation(async (command: string, args?: { request?: unknown }) => {
      if (command === "query_saved_list") return saved;
      if (command === "query_saved_save") {
        savedRequests.push(args?.request);
        return { query: { ...saved[0]!, ...(args?.request as object), id: "s1", updatedAt: "2026-10-20T10:00:00Z" }, created: savedRequests.length === 1 };
      }
      throw new Error(command);
    });
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-save",
        draft: "SELECT 99",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("query_saved_list");
    await wrapper.get("[data-action='saved']").trigger("click");
    await flushPromises();
    expect(wrapper.get("[data-action='open-saved']").text()).toContain("周报");
    await wrapper.get("form.pop-form").trigger("submit");
    await flushPromises();
    expect(savedRequests).toEqual([]);
    expect(wrapper.text()).toContain("请填写名称");
    await wrapper.get("[data-action='saved-name']").setValue("每日统计");
    await wrapper.get("form.pop-form").trigger("submit");
    await flushPromises();
    expect(savedRequests).toEqual([{ sql: "SELECT 99", connectionId: "shop", connectionName: "Shop", databaseName: "shop_staging", engine: "mysql", name: "每日统计" }]);
    expect(wrapper.text()).toContain("已保存「每日统计」");
    await wrapper.get("[data-action='saved-name']").setValue("每日统计");
    await wrapper.get("form.pop-form").trigger("submit");
    await flushPromises();
    expect(savedRequests).toHaveLength(2);
    expect(wrapper.text()).toContain("已覆盖更新「每日统计」");
    await wrapper.get("[data-action='open-saved']").trigger("click");
    await flushPromises();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    expect(view.state.doc.toString()).toBe("SELECT * FROM orders WHERE week = 42;");
    expect(wrapper.emitted("update:draft")?.at(-1)).toEqual(["query-save", "SELECT * FROM orders WHERE week = 42;"]);
    expect(wrapper.emitted("target")?.[0]?.[0]).toMatchObject({ connectionId: "atlas", database: "atlas_dev", status: "connected" });
    expect(invoke.mock.calls.some((call) => call[0] === "query_start")).toBe(false);
    wrapper.unmount();
  });

  it("keeps run visible and lists rare toolbar actions as text in the more menu", async () => {
    replaceCatalog(catalogFixture());
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-more-tools",
        draft: "select  1",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    expect(wrapper.get("[data-action='run']").text()).toContain("执行");
    expect(wrapper.find("[data-action='run-menu']").exists()).toBe(true);
    expect(wrapper.get(".q-toolbar .conn-state").text().length).toBeGreaterThan(0);
    expect(wrapper.find("[data-menu='more-tools']").exists()).toBe(false);
    await wrapper.get("[data-action='more-tools']").trigger("click");
    const menu = wrapper.get("[data-menu='more-tools']");
    expect(menu.findAll("[role='menuitem']").map((item) => item.text().trim())).toEqual([
      "格式化",
      "大纲",
      "打开",
      "历史",
      "保存",
      "执行计划",
      "实际执行并分析",
    ]);
    expect(menu.find("[data-action='run']").exists()).toBe(false);
    expect(wrapper.find(".run-group [data-action='run']").exists()).toBe(true);
    await menu.get("[data-action='outline']").trigger("click");
    expect(wrapper.find("[data-menu='more-tools']").exists()).toBe(false);
    expect(wrapper.get("[data-action='outline']").attributes("aria-pressed")).toBe("true");
    await wrapper.get("[data-action='more-tools']").trigger("click");
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    await wrapper.get("[data-menu='more-tools'] [data-action='format']").trigger("click");
    expect(view.state.doc.toString()).not.toBe("select  1");
    expect(wrapper.find("[data-action='run']").exists()).toBe(true);
    const badge = wrapper.get(".results-heading > .badge");
    expect(badge.text()).toContain("只读结果");
    const reason = (wrapper.getCurrentComponent() as unknown as { provides: { resultEditReason: Ref<string> } }).provides.resultEditReason;
    reason.value = "按 id 定位行，查询结果不能新增行";
    await nextTick();
    expect(badge.attributes("title")).toBe("按 id 定位行，查询结果不能新增行");
    wrapper.unmount();
  });

  it("hides more tools in a wide pane and clips the target before the connection status", () => {
    const style = readFileSync(resolve(dirname(fileURLToPath(import.meta.url)), "QueryTab.vue"), "utf8");
    const popAt = style.indexOf(".q-pop { position: relative; display: inline-flex; }");
    const hideAt = style.indexOf(".q-toolbar .q-pop.q-more-tools { display: none; flex-shrink: 0; }");
    const containerAt = style.indexOf("@container query-pane (max-width: 720px)");
    expect(popAt).toBeGreaterThan(0);
    expect(hideAt).toBeGreaterThan(popAt);
    expect(containerAt).toBeGreaterThan(hideAt);
    const containerBlock = style.slice(containerAt, style.indexOf("@media (max-width: 800px)", containerAt));
    expect(containerBlock).toContain(".q-toolbar .q-pop.q-more-tools { display: inline-flex; }");
    const targetBtn = style.slice(style.indexOf(".target-btn {"), style.indexOf("}", style.indexOf(".target-btn {")));
    expect(style.indexOf(".target-btn {")).toBeLessThan(containerAt);
    expect(targetBtn).toContain("overflow: hidden");
    expect(targetBtn).toContain("max-width: 100%");
    expect(targetBtn).toContain("min-width: 0");
    expect(style).toContain(".q-toolbar { display: flex; gap: var(--sp-2); align-items: center; flex-wrap: nowrap;");
    expect(style).toContain(".q-toolbar .conn-state { flex-shrink: 0; }");
    expect(style).not.toContain(".q-toolbar .conn-state { display: none; }");
  });
});

describe("tsql statement split", () => {
  it("keeps top-level BEGIN...END blocks together and closes at END", () => {
    const parts = splitSql("BEGIN\n  UPDATE t SET n = 1;\n  UPDATE u SET m = 2;\nEND\nSELECT 1;", "sqlserver");
    expect(parts).toHaveLength(2);
    expect(parts[0]!.text.startsWith("BEGIN")).toBe(true);
    expect(parts[0]!.text.trimEnd().endsWith("END")).toBe(true);
    expect(parts[1]!.text.replace(/;$/, "")).toBe("SELECT 1");

    // 嵌套块是一条语句。
    expect(splitSql("BEGIN\n  BEGIN\n    SELECT 1;\n  END\n  SELECT 2;\nEND;", "sqlserver")).toHaveLength(1);

    // 字符串里的分号不切分。
    expect(splitSql("BEGIN\n  SELECT 'a;b';\nEND;", "sqlserver")).toHaveLength(1);

    // END 后跟 ELSE 时不提前闭合（IF ... ELSE）。
    const ifElse = inspectSql("IF 1 = 1 BEGIN SELECT 1; END ELSE BEGIN SELECT 2; END", "sqlserver");
    expect(ifElse.pieces).toHaveLength(1);
    expect(ifElse.refusal).toBeNull();

    // BEGIN TRAN 是事务语句，不是块。
    expect(splitSql("BEGIN TRAN;\nUPDATE t SET n = 1;\nCOMMIT;", "sqlserver")).toHaveLength(3);
    expect(splitSql("BEGIN TRANSACTION;\nSELECT 1;", "sqlserver")).toHaveLength(2);

    // 未闭合块整批拒绝。
    const unclosed = inspectSql("SELECT 1;\nBEGIN\n  SELECT 2;", "sqlserver");
    expect(unclosed.pieces).toHaveLength(1);
    expect(unclosed.refusal).toBe("BEGIN 块未闭合，没有发送。");

    // DELIMITER 是 mysql 客户端指令，T-SQL 跳过并提示。
    const delimiter = inspectSql("DELIMITER //\nSELECT 1;", "sqlserver");
    expect(delimiter.pieces).toHaveLength(1);
    expect(delimiter.notices[0]).toContain("SQL Server 不使用 DELIMITER");
  });

  it("treats bracket identifiers, TRY/CATCH and GO batches like the Rust splitter", () => {
    // 方括号标识符：[end]/[case]/[a]]b] 不参与块计数与切分。
    const brackets = splitSql("SELECT [end], [case], [a]]b] FROM t; SELECT 1;", "sqlserver");
    expect(brackets).toHaveLength(2);
    expect(brackets[0]!.text).toContain("[a]]b]");
    const unclosed = inspectSql("SELECT [name FROM t;", "sqlserver");
    expect(unclosed.refusal).toBe("方括号标识符没有闭合，没有发送。");
    // 方括号标识符里的 -- 不是注释（与 Rust 镜像同序）：标识符完整保留、正常切分。
    const dashed = splitSql("SELECT [a--b] FROM t; SELECT 1;", "sqlserver");
    expect(dashed).toHaveLength(2);
    expect(dashed[0]!.text).toContain("[a--b]");

    // TRY/CATCH 是一条语句：END TRY 后的分号不切分，END CATCH 才收束。
    const tryCatch = splitSql("BEGIN TRY\n  SELECT 1;\nEND TRY;\nBEGIN CATCH\n  SELECT 2;\nEND CATCH\nSELECT 3;", "sqlserver");
    expect(tryCatch).toHaveLength(2);
    expect(tryCatch[0]!.text.trimEnd().endsWith("END CATCH")).toBe(true);

    // GO 是批次分隔：不进语句文本；GO n 计数形式同样跳过；行中的 GO 只是普通词。
    const batches = splitSql("SELECT 1;\nGO\nSELECT 2\nGO 5\nSELECT 3;", "sqlserver");
    expect(batches).toHaveLength(3);
    expect(batches.every((piece) => !piece.text.includes("GO"))).toBe(true);
    expect(splitSql("SELECT 1 FROM gopher WHERE x = 1;", "sqlserver")).toHaveLength(1);
    const noSemi = splitSql("UPDATE t SET n = 1\nGO\nUPDATE u SET m = 2\nGO", "sqlserver");
    expect(noSemi).toHaveLength(2);
  });

  it("inserts a tree reference at the cursor as one undo step", async () => {
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-insert",
        draft: "SELECT * FROM orders o WHERE ",
        connectionId: "atlas",
        connectionName: "Atlas",
        database: "atlas_dev",
        engine: "postgres",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement);
    expect(view).toBeTruthy();
    const cursor = view!.state.doc.length;
    view!.dispatch({ selection: { anchor: cursor } });
    setEditorFocus("query-insert", true);

    const outcome = insertAtCursor("query-insert", {
      connectionId: "atlas",
      connectionName: "Atlas",
      engine: "postgres",
      database: "atlas_dev",
      schema: "public",
      table: "orders",
      column: "status",
    });
    expect(outcome).toBe("ok");
    expect(view!.state.doc.toString()).toBe("SELECT * FROM orders o WHERE o.status");
    expect(view!.state.selection.main.head).toBe(cursor + "o.status".length);
    expect(wrapper.emitted("update:draft")?.at(-1)?.[1]).toBe("SELECT * FROM orders o WHERE o.status");

    // 一次撤销回到插入前，不残留半截限定名。
    undo(view!);
    expect(view!.state.doc.toString()).toBe("SELECT * FROM orders o WHERE ");
    undo(view!);

    // 执行中的标签不接收插入。
    adoptRun(
      "query-insert",
      runDto({ status: "running", connectionId: "atlas", connectionName: "Atlas", database: "atlas_dev", engine: "postgres" }),
    );
    await nextTick();
    expect(
      insertAtCursor("query-insert", {
        connectionId: "atlas",
        connectionName: "Atlas",
        engine: "postgres",
        database: "atlas_dev",
        schema: "public",
        table: "orders",
        column: "status",
      }),
    ).toBe("running");
    expect(view!.state.doc.toString()).toBe("SELECT * FROM orders o WHERE ");

    wrapper.unmount();
    expect(
      insertAtCursor("query-insert", {
        connectionId: "atlas",
        connectionName: "Atlas",
        engine: "postgres",
        database: "atlas_dev",
        schema: "public",
        table: "orders",
        column: "status",
      }),
    ).toBe("none");
    resetEditors();
  });
});

describe("completion frequency wiring", () => {
  function shopTab(tabId: string, draft: string) {
    return mount(QueryTab, {
      props: {
        tabId,
        draft,
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
  }

  it("orders same-class candidates by the stored identifier frequency", async () => {
    replaceCatalog(catalogFixture());
    localStorage.clear();
    bumpIdentifiers("SELECT state FROM orders");
    bumpIdentifiers("SELECT state FROM orders");
    invoke.mockImplementation(async (command: string) => {
      if (command !== "grid_describe") throw new Error(command);
      return {
        engine: "mysql",
        schema: "",
        name: "orders",
        kind: "table",
        key: ["id"],
        columns: [
          { name: "status", typeName: "varchar", nullable: false, primary: false, unique: false, auto: false },
          { name: "state", typeName: "varchar", nullable: false, primary: false, unique: false, auto: false },
        ],
      };
    });
    const wrapper = shopTab("query-frequency-order", "SELECT st FROM orders");
    await nextTick();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    view.dispatch({ selection: { anchor: "SELECT st".length } });
    startCompletion(view);
    await vi.waitFor(() => {
      expect(currentCompletions(view.state).some((item) => item.label === "state")).toBe(true);
    });
    const labels = currentCompletions(view.state).map((item) => item.label);
    expect(labels.indexOf("state")).toBeLessThan(labels.indexOf("status"));
    wrapper.unmount();
    clearColumnCache();
    localStorage.clear();
  });

  it("counts identifiers from a successful run and stores no SQL text", async () => {
    replaceCatalog(catalogFixture());
    localStorage.clear();
    invoke.mockImplementation(async (command: string, args?: { request?: { sql?: string } }) => {
      if (command !== "query_start") throw new Error(command);
      return runDto({
        status: "done",
        statements: [{
          sql: args?.request?.sql ?? "",
          status: "ok",
          message: "",
          columns: [],
          rows: [],
          truncated: false,
          affected: 1,
        }],
      });
    });
    const wrapper = shopTab("query-frequency-run", "UPDATE orders SET state = 'paid' WHERE order_no = 7");
    await nextTick();
    await wrapper.get("[data-action='run']").trigger("click");
    await flushPromises();
    const stored = loadFrequency();
    expect(stored).toEqual({ orders: 1, state: 1, order_no: 1 });
    expect(stored).not.toHaveProperty("paid");
    expect(stored).not.toHaveProperty("where");
    expect(stored).not.toHaveProperty("update");
    expect(localStorage.getItem(FREQUENCY_KEY) ?? "").not.toContain("UPDATE orders SET");
    wrapper.unmount();
    localStorage.clear();
  });
});
describe("editor caret and selection stability", () => {
  function queryTab(draft: string, tabId: string) {
    replaceCatalog(catalogFixture());
    return mount(QueryTab, {
      props: {
        tabId,
        draft,
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
  }

  // 这里用 EditorState 上的 selection 直接定位「当前光标」，不走鼠标事件，
  // 因此它只覆盖输入/撤销/重做/删除与光标的关系；真实鼠标点击定位由 T2 的浏览器与原生验收覆盖。
  it("keeps typing, undo, redo and delete on the current caret position", async () => {
    const sql = "-- 订单筛选\nSELECT id, 名称 FROM orders;\nSELECT 2;";
    const wrapper = queryTab(sql, "query-caret");
    await nextTick();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    const caret = sql.indexOf("名称") + 1;
    view.dispatch({ selection: { anchor: caret } });
    await nextTick();

    // 输入落在当前光标处，光标停在新字符之后，其余字符不移动。
    view.dispatch(view.state.replaceSelection("新"));
    await nextTick();
    const typed = sql.slice(0, caret) + "新" + sql.slice(caret);
    expect(view.state.doc.toString()).toBe(typed);
    expect(view.state.selection.main.head).toBe(caret + 1);
    const drafts = () => (wrapper.emitted("update:draft") ?? []).map((call) => call[1]);
    expect(drafts()).toEqual([typed]);

    // 撤销回到原文：内容与光标一起回退，不丢字符也不重复。
    undo(view);
    await nextTick();
    expect(view.state.doc.toString()).toBe(sql);
    expect(view.state.selection.main.head).toBe(caret);
    // 撤销回已提交的草稿：不再重复回写同一份文本。
    expect(drafts()).toEqual([typed]);

    // 重做再次落回输入后的内容与光标位置。
    redo(view);
    await nextTick();
    expect(view.state.doc.toString()).toBe(typed);
    expect(view.state.selection.main.head).toBe(caret + 1);
    expect(drafts()).toEqual([typed, typed]);

    // 删除只影响光标所在字符，中文与注释行保持原样。
    view.dispatch({ changes: { from: caret, to: caret + 1 } });
    await nextTick();
    expect(view.state.doc.toString()).toBe(sql);
    expect(view.state.selection.main.head).toBe(caret);
    wrapper.unmount();
  });

  it("drops the caret to the end of the formatted document when formatting reflows lines", async () => {
    const sql = "select  1;\nselect   2;\nselect 3;";
    const wrapper = queryTab(sql, "query-format-caret");
    await nextTick();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    view.dispatch({ selection: { anchor: sql.indexOf("select   2;") + 8 } });
    await wrapper.get("[data-action='format']").trigger("click");
    const next = view.state.doc.toString();
    // 每条语句都被拆成「关键字行 + 内容行」：先钉住格式化结果本身。
    expect(next).toBe("SELECT\n  1;\nSELECT\n  2;\nSELECT\n  3;");
    const head = view.state.selection.main.head;
    // 行数变化后原「行号 + 列」不再成立，光标退回到格式化文档末尾（"3;" 之后）。
    expect(head).toBe(next.length);
    expect(view.state.doc.lineAt(head).number).toBe(view.state.doc.lines);
    // 明确排除两种错位：上一条语句行尾、以及 SELECT 与列清单之间。
    expect(head).not.toBe(next.indexOf("  1;") + "  1;".length);
    expect(head).not.toBe(next.indexOf("  2;") + "  2;".length);
    expect(head).not.toBe("SELECT".length);
    wrapper.unmount();
  });

  it("keeps an end-of-draft caret at the end after formatting a single line", async () => {
    const sql = "select id, name from t";
    const wrapper = queryTab(sql, "query-format-caret-end");
    await nextTick();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    view.dispatch({ selection: { anchor: sql.length } });
    await wrapper.get("[data-action='format']").trigger("click");
    const next = view.state.doc.toString();
    expect(next).toBe("SELECT\n  id,\n  name\nFROM\n  t");
    const head = view.state.selection.main.head;
    // 一行拆成五行：光标落在格式化文档末尾，不停在 SELECT 之后或列清单中间。
    expect(head).toBe(next.length);
    expect(head).not.toBe("SELECT".length);
    expect(head).not.toBe(next.indexOf("  id,") + 1);
    wrapper.unmount();
  });

  it("falls back to the document end when formatting trims the caret column", async () => {
    const sql = "select\n  id   \nfrom\n  t";
    const wrapper = queryTab(sql, "query-format-caret-column");
    await nextTick();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    // 光标停在行尾空白里：格式化会删掉这段空白，原列偏移随之越界。
    view.dispatch({ selection: { anchor: sql.indexOf("  id   ") + "  id   ".length } });
    await wrapper.get("[data-action='format']").trigger("click");
    const next = view.state.doc.toString();
    expect(next).toBe("SELECT\n  id\nFROM\n  t");
    // 行数没变，但列越界：同样退回到末尾，不停在被截短后的行尾。
    expect(view.state.selection.main.head).toBe(next.length);
    wrapper.unmount();
  });

  it("keeps the caret on the same line when formatting leaves the line count untouched", async () => {
    const sql = "select\n  id\nfrom\n  t";
    const wrapper = queryTab(sql, "query-format-caret-same-line");
    await nextTick();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    view.dispatch({ selection: { anchor: sql.indexOf("id") } });
    await wrapper.get("[data-action='format']").trigger("click");
    const next = view.state.doc.toString();
    // 只改关键字大小写：行数与列都不变，光标仍在 id 之前。
    expect(next).toBe("SELECT\n  id\nFROM\n  t");
    expect(view.state.selection.main.head).toBe(next.indexOf("id"));
    wrapper.unmount();
  });

  it("keeps the caret column when formatting only rewrites ASCII case before it", async () => {
    const sql = "select\n  id\nfrom\n  t";
    const wrapper = queryTab(sql, "query-format-caret-case");
    await nextTick();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    // 光标停在首行 select 的中间（"sel|ect"）：格式化只把这个关键字改写成大写。
    const anchor = 3;
    expect(sql.slice(anchor)).toBe("ect\n  id\nfrom\n  t");
    view.dispatch({ selection: { anchor } });
    await wrapper.get("[data-action='format']").trigger("click");
    const next = view.state.doc.toString();
    expect(next).toBe("SELECT\n  id\nFROM\n  t");
    // 行数没变、列没越界，且光标前的文本只有 ASCII 大小写差异（sel → SEL），所以保留同一列。
    expect(next.slice(0, anchor)).toBe("SEL");
    expect(view.state.selection.main.head).toBe(anchor);
    // 排除退到末尾与退到行首两种错位。
    expect(view.state.selection.main.head).not.toBe(next.length);
    expect(view.state.selection.main.head).not.toBe(0);
    wrapper.unmount();
  });

  it("drops the caret to the document end when formatting rewrites the indentation before it", async () => {
    // 四条语句各自成行，格式化后行数依旧是 4，原光标列 4 也没有越界；
    // 但光标前的缩进从「四个空格」被重排成「两个空格」，同一列已经从 id 之前挪到 id 之后。
    const sql = "select\n    id\nfrom\n  t";
    const wrapper = queryTab(sql, "query-format-caret-indent");
    await nextTick();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    const caret = sql.indexOf("id");
    view.dispatch({ selection: { anchor: caret } });
    await wrapper.get("[data-action='format']").trigger("click");
    const next = view.state.doc.toString();
    expect(next).toBe("SELECT\n  id\nFROM\n  t");
    // 前提确认：行数没变、原列没越界，所以这个用例命中的正是「光标前文本被改写」这一条守卫。
    expect(next.split("\n")).toHaveLength(sql.split("\n").length);
    expect(next.split("\n")[1]!.length).toBeGreaterThanOrEqual("    ".length);
    const head = view.state.selection.main.head;
    expect(head).toBe(next.length);
    // 明确排除「同列」错位结果：id 之后（旧逻辑会落在这里）。
    expect(head).not.toBe(next.indexOf("id") + "id".length);
    expect(head).not.toBe(next.indexOf("id"));
    wrapper.unmount();
  });

  it("puts the caret at the start when a history entry replaces the whole draft", async () => {
    replaceCatalog(catalogFixture());
    invoke.mockImplementation(async (command: string) => {
      if (command === "query_history") {
        return [{
          id: "h1",
          sql: "SELECT count(*) FROM orders;",
          connectionId: "shop",
          connectionName: "Shop",
          databaseName: "shop_staging",
          engine: "mysql",
          startedAt: "2026-10-20T09:30:00Z",
          finishedAt: "2026-10-20T09:30:01Z",
          status: "ok",
          elapsedMs: 34,
          summary: "1 行",
        }];
      }
      throw new Error(command);
    });
    const wrapper = mount(QueryTab, {
      props: {
        tabId: "query-history-caret",
        draft: "SELECT 1",
        connectionId: "shop",
        connectionName: "Shop",
        database: "shop_staging",
        engine: "mysql",
        connectionStatus: "connected",
        notice: null,
      },
    });
    await nextTick();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    view.dispatch({ selection: { anchor: 8 } });
    await wrapper.get("[data-action='history']").trigger("click");
    await flushPromises();
    await wrapper.get("[data-action='open-history']").trigger("click");
    await flushPromises();
    expect(view.state.doc.toString()).toBe("SELECT count(*) FROM orders;");
    expect(view.state.selection.main.head).toBe(0);
    // 载入后从开头继续编辑，不再追加到末尾。
    view.dispatch(view.state.replaceSelection("-- 补充\n"));
    expect(view.state.doc.toString().startsWith("-- 补充\nSELECT")).toBe(true);
    wrapper.unmount();
  });

  it("keeps the selection and the editor document when the run status refreshes", async () => {
    const sql = "SELECT 1;\nSELECT 2;";
    const wrapper = queryTab(sql, "query-refresh");
    await nextTick();
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)!;
    const element = wrapper.get(".cm-content").element;
    const docBefore = view.state.doc;
    view.dispatch({ selection: { anchor: 9, head: 18 } });
    await nextTick();
    adoptRun("query-refresh", runDto({ status: "running", statements: [] }));
    await nextTick();
    adoptRun("query-refresh", runDto({
      status: "done",
      statements: [{ sql: "SELECT 2;", status: "ok", message: "", columns: ["n"], rows: [["2"]], truncated: false, affected: 0 }],
    }));
    await flushPromises();
    // 状态刷新只改结果区：同一个编辑器、同一份文档、同一段选区。
    const range = view.state.selection.main;
    expect([range.from, range.to]).toEqual([9, 18]);
    expect(view.state.doc).toBe(docBefore);
    expect(EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement)).toBe(view);
    expect(wrapper.get(".cm-content").element).toBe(element);
    expect(wrapper.get("[data-action='run']").text()).toContain("执行选中");
    wrapper.unmount();
  });
});
