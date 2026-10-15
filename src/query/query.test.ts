import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import { catalogFixture } from "../catalog/fixtures";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import { createSession } from "../shell/session";
import type { RunDto } from "./api";
import { completionPlan } from "./complete";
import { resolveEditor } from "./editor";
import QueryTab from "./QueryTab.vue";
import { adoptRun, attachCloser, connectionBusy, guardClose, requestCancel, resetQueryRuntime, startTabRun, tabRun } from "./runtime";
import { QUERY_EDITOR_KEYS, sqlToRun } from "./scope";
import { splitSql, statementAt } from "./split";

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
  invoke.mockReset();
});

describe("query editing", () => {
  it("splits the current statement without taking the previous comment", () => {
    const sql = "SELECT 'a;b';\n-- c;\nUPDATE t SET n = 1;\nSELECT $$do;$$;";
    const parts = splitSql(sql);
    expect(parts.map((part) => part.text)).toEqual(["SELECT 'a;b';", "UPDATE t SET n = 1;", "SELECT $$do;$$;"]);
    expect(statementAt(sql, parts[1]!.start + 2)?.text).toBe("UPDATE t SET n = 1;");
    expect(sqlToRun(sql, { from: parts[1]!.start, to: parts[1]!.end }, "auto")).toEqual({ sql: "UPDATE t SET n = 1;", mode: "selection" });
    expect(sqlToRun(sql, { from: parts[1]!.start + 2, to: parts[1]!.start + 2 }, "current").sql).toBe("UPDATE t SET n = 1;");
    expect(sqlToRun(sql, { from: 0, to: 0 }, "auto").sql).toBe(sql);
  });

  it("keeps desktop editor shortcuts on Ctrl or Command", () => {
    expect(QUERY_EDITOR_KEYS).toEqual(["Mod-Enter", "Mod-Shift-Enter", "Mod-.", "Ctrl-Space", "Mod-/"]);
    expect(QUERY_EDITOR_KEYS).not.toContain("Alt-W");
    expect(QUERY_EDITOR_KEYS).not.toContain("Ctrl-Alt-N");
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
  });

  it("shows the connection and database, and sends only the chosen sql", async () => {
    replaceCatalog(catalogFixture());
    const calls: { command: string; sql?: string }[] = [];
    invoke.mockImplementation(async (command: string, args?: { request?: { sql: string } }) => {
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
    expect(wrapper.get("[aria-label='查询']").attributes("data-tab-id")).toBe("query-1");
    expect(wrapper.get("[data-action='target']").attributes("aria-label")).toContain("执行目标");
    expect(wrapper.get("[data-action='target']").text()).toContain("Shop / shop_staging");
    expect(wrapper.get("[data-action='target']").text()).toContain("MY");
    expect(wrapper.text()).toContain("已连接");
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element);
    expect(view).toBeTruthy();
    const from = sql.indexOf("SELECT 2;");
    view!.dispatch({ selection: { anchor: from, head: from + "SELECT 2;".length } });
    await nextTick();
    await wrapper.get("[data-action='run']").trigger("click");
    await flushPromises();
    const started = () => calls.filter((call) => call.command === "query_start");
    expect(started().at(-1)).toMatchObject({ command: "query_start", sql: "SELECT 2;" });
    expect(wrapper.text()).toContain("已读取的表和视图");

    view!.dispatch({ selection: { anchor: 0, head: 0 } });
    await nextTick();
    await wrapper.get("[data-action='run-menu']").trigger("click");
    await wrapper.get("[data-action='run-current']").trigger("click");
    await flushPromises();
    expect(started().at(-1)).toMatchObject({ command: "query_start", sql: "SELECT 1;" });
    wrapper.unmount();
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
    EditorView.findFromDOM(wrapper.get(".cm-content").element)?.destroy();
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
});
