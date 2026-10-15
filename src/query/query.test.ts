import { currentCompletions, startCompletion } from "@codemirror/autocomplete";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import { catalogFixture } from "../catalog/fixtures";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import { createSession } from "../shell/session";
import type { RunDto } from "./api";
import { clearColumnCache, qualifierFor } from "./columns";
import { completionPlan, metadataNote } from "./complete";
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
  clearColumnCache();
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
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element);
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
    const failedView = EditorView.findFromDOM(failed.get(".cm-content").element)!;
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
    expect(missing.get(".q-hint").text()).toContain("字段不可用");
    invoke.mockClear();
    const missingView = EditorView.findFromDOM(missing.get(".cm-content").element)!;
    missingView.dispatch({ selection: { anchor: missingView.state.doc.length } });
    startCompletion(missingView);
    await new Promise((resolve) => setTimeout(resolve, 120));
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
    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element)!;
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
    const brokenView = EditorView.findFromDOM(broken.get(".cm-content").element)!;
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
    const commentView = EditorView.findFromDOM(commentTab.get(".cm-content").element)!;
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
    const glueView = EditorView.findFromDOM(glueTab.get(".cm-content").element)!;
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
    expect(calls[0]?.sql).toBe("EXPLAIN SELECT * FROM orders");
    expect(calls[0]?.sql?.toLowerCase()).not.toContain("analyze");
    expect(wrapper.get("[data-result='plan']").text()).toContain("Seq Scan on orders");
    expect(wrapper.get("[data-result='plan']").text()).toContain("未使用 ANALYZE");
    expect(wrapper.find("[aria-label='查询结果']").exists()).toBe(false);

    const view = EditorView.findFromDOM(wrapper.get(".cm-content").element)!;
    view.dispatch({ selection: { anchor: 0, head: sql.length } });
    await wrapper.get("[data-action='plan']").trigger("click");
    await flushPromises();
    expect(calls).toHaveLength(1);
    expect(wrapper.get("[role='alert']").text()).toContain("一条");
    wrapper.unmount();

    const sqliteCalls: string[] = [];
    invoke.mockImplementation(async (command: string, args?: { request?: { sql: string } }) => {
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
});
