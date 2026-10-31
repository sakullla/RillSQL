import { afterEach, describe, expect, it } from "vitest";
import { createSession, restoreSession, snapshotSession, type SessionSnapshotTab } from "./session";

function snapTab(overrides: Partial<SessionSnapshotTab> = {}): SessionSnapshotTab {
  return {
    id: "query-1",
    title: "查询 1",
    draft: "",
    connectionId: null,
    connectionName: null,
    database: null,
    engine: null,
    connectionStatus: null,
    targetText: null,
    targetNotice: null,
    path: null,
    ...overrides,
  };
}

afterEach(() => {
  location.hash = "";
});

describe("session persistence", () => {
  it("round-trips query tabs with drafts and targets through snapshot and restore", () => {
    const session = createSession();
    session.openQueryDraft({
      sql: "select 1",
      connectionId: "shop",
      connectionName: "书店",
      database: "shop",
      engine: "postgres",
      status: "connected",
      needsTarget: false,
      notice: null,
    });
    session.openQuery();
    const first = session.tabs.value[0]!;
    const second = session.tabs.value[1]!;
    session.activeId.value = first.id;

    const restored = restoreSession(JSON.stringify(snapshotSession(session.tabs.value, session.activeId.value)));
    expect(restored).not.toBeNull();

    const next = createSession("", restored);
    expect(next.tabs.value.map((tab) => tab.id)).toEqual([first.id, second.id]);
    expect(next.activeId.value).toBe(first.id);
    const revived = next.tabs.value[0]!;
    expect(revived.kind).toBe("query");
    expect(revived.title).toBe(first.title);
    expect(revived.draft).toBe("select 1");
    expect(revived.connectionId).toBe("shop");
    expect(revived.connectionName).toBe("书店");
    expect(revived.database).toBe("shop");
    expect(revived.engine).toBe("postgres");
    expect(revived.connectionStatus).toBe("connected");
    expect(revived.targetText).toBe("书店 / shop");
  });

  it("snapshots all five tab kinds and keeps a non-query active tab", () => {
    const session = createSession();
    session.openQuery();
    session.openTable({ id: "shop:shop:public:customers", label: "customers", path: "书店 / shop / public", pane: "structure" });
    session.tabs.value.find((tab) => tab.kind === "table")!.structureSection = "indexes";
    session.openStructure({ id: "placeholder", label: "新建表结构" });
    session.openDefinition({
      id: "shop:shop::add_customer:function::",
      label: "add_customer",
      connectionId: "shop",
      connectionName: "书店",
      database: "shop",
      engine: "mysql",
      status: "connected",
      schema: "",
      name: "add_customer",
      kind: "function",
      signature: "(p int)",
      relation: "",
    });
    session.openDiagram({
      id: "shop:shop:public",
      label: "public 关系图",
      path: "书店 / shop / public",
      connectionId: "shop",
      connectionName: "书店",
      database: "shop",
      engine: "postgres",
      status: "connected",
      schema: "public",
    });
    const tableId = session.tabs.value.find((tab) => tab.kind === "table")!.id;
    session.activeId.value = tableId;

    const snapshot = snapshotSession(session.tabs.value, session.activeId.value);
    expect(snapshot.tabs.map((tab) => tab.kind)).toEqual(["query", "table", "structure", "definition", "diagram"]);
    expect(snapshot.activeId).toBe(tableId);
  });

  it("round-trips a table tab with pane, section, path and dangling notice", () => {
    const session = createSession();
    session.openTable({ id: "shop:shop:public:customers", label: "customers", path: "书店 / shop / public", pane: "structure" });
    const tab = session.tabs.value[0]!;
    tab.structureSection = "indexes";
    tab.targetNotice = "这个标签指向的连接已删除。";

    const restored = restoreSession(JSON.stringify(snapshotSession(session.tabs.value, tab.id)));
    expect(restored).not.toBeNull();
    const next = createSession("", restored);
    const revived = next.tabs.value[0]!;
    expect(revived.kind).toBe("table");
    expect(revived.id).toBe("table:shop:shop:public:customers");
    expect(revived.title).toBe("customers");
    expect(revived.path).toBe("书店 / shop / public");
    expect(revived.tablePane).toBe("structure");
    expect(revived.structureSection).toBe("indexes");
    expect(revived.targetNotice).toBe("这个标签指向的连接已删除。");
    // 表标签的 draft 只是占位：没有草稿语义，也不参与关闭守卫
    expect(revived.draft).toBe("");
    expect(revived.lastExecutedSql).toBeNull();
  });

  it("round-trips a definition tab with its target object", () => {
    const session = createSession();
    session.openDefinition({
      id: "shop:shop::add_customer:function:(p int):",
      label: "add_customer",
      path: "书店 / shop",
      connectionId: "shop",
      connectionName: "书店",
      database: "shop",
      engine: "mysql",
      status: "connected",
      schema: "",
      name: "add_customer",
      kind: "function",
      signature: "(p int)",
      relation: "",
    });

    const restored = restoreSession(JSON.stringify(snapshotSession(session.tabs.value, session.tabs.value[0]!.id)));
    expect(restored).not.toBeNull();
    const next = createSession("", restored);
    const revived = next.tabs.value[0]!;
    expect(revived.kind).toBe("definition");
    expect(revived.connectionId).toBe("shop");
    expect(revived.database).toBe("shop");
    expect(revived.engine).toBe("mysql");
    expect(revived.definition).toEqual({ schema: "", name: "add_customer", kind: "function", signature: "(p int)", relation: "" });
  });

  it("round-trips a diagram tab with its schema", () => {
    const session = createSession();
    session.openDiagram({
      id: "shop:shop:public",
      label: "public 关系图",
      path: "书店 / shop",
      connectionId: "shop",
      connectionName: "书店",
      database: "shop",
      engine: "postgres",
      status: "connected",
      schema: "public",
    });

    const restored = restoreSession(JSON.stringify(snapshotSession(session.tabs.value, session.tabs.value[0]!.id)));
    expect(restored).not.toBeNull();
    const next = createSession("", restored);
    const revived = next.tabs.value[0]!;
    expect(revived.kind).toBe("diagram");
    expect(revived.connectionId).toBe("shop");
    expect(revived.diagram).toEqual({ schema: "public" });
    expect(revived.targetText).toBe("书店 / shop / public");
  });

  it("round-trips the structure placeholder tab", () => {
    const session = createSession();
    session.openStructure({ id: "placeholder", label: "新建表结构" });

    const restored = restoreSession(JSON.stringify(snapshotSession(session.tabs.value, session.tabs.value[0]!.id)));
    expect(restored).not.toBeNull();
    const next = createSession("", restored);
    const revived = next.tabs.value[0]!;
    expect(revived.kind).toBe("structure");
    expect(revived.id).toBe("structure:placeholder");
    expect(revived.title).toBe("新建表结构");
  });

  it("treats legacy snapshots without kind as query tabs", () => {
    const restored = restoreSession(JSON.stringify({ tabs: [snapTab({ id: "query-3", title: "查询 3", draft: "select 3" })], activeId: "query-3" }));
    expect(restored).not.toBeNull();
    const session = createSession("", restored);
    expect(session.tabs.value[0]!.kind).toBe("query");
    expect(session.tabs.value[0]!.draft).toBe("select 3");
  });

  it("returns null for missing or malformed session data", () => {
    expect(restoreSession(null)).toBeNull();
    expect(restoreSession("")).toBeNull();
    expect(restoreSession("not json")).toBeNull();
    expect(restoreSession("[]")).toBeNull();
    expect(restoreSession('{"tabs":"x"}')).toBeNull();
    expect(restoreSession('{"tabs":[null]}')).toBeNull();
    expect(restoreSession('{"tabs":[{"id":"","title":"查询 1","draft":""}]}')).toBeNull();
    expect(restoreSession('{"tabs":[{"id":"query-1","title":5,"draft":""}]}')).toBeNull();
    expect(restoreSession('{"tabs":[{"id":"query-1","title":"查询 1","draft":"select 1","database":7}]}')).toBeNull();
    expect(restoreSession('{"tabs":[{"id":"query-1","title":"查询 1","draft":"select 1","connectionStatus":"oops"}]}')).toBeNull();
    expect(restoreSession('{"tabs":[{"id":"query-1","title":"查询 1","draft":"select 1","kind":"nope"}]}')).toBeNull();
    expect(restoreSession('{"tabs":[{"id":"t1","title":"t","draft":"","kind":"table","tablePane":"oops"}]}')).toBeNull();
    expect(restoreSession('{"tabs":[{"id":"t1","title":"t","draft":"","kind":"table","structureSection":"oops"}]}')).toBeNull();
    expect(restoreSession('{"tabs":[{"id":"d1","title":"d","draft":"","kind":"definition","definition":{"schema":1}}]}')).toBeNull();
    expect(restoreSession('{"tabs":[{"id":"g1","title":"g","draft":"","kind":"diagram","diagramSchema":9}]}')).toBeNull();
    expect(restoreSession('{"tabs":[{"id":"q1","title":"q","draft":"","path":3}]}')).toBeNull();
    expect(restoreSession('{"tabs":[],"activeId":3}')).toBeNull();
  });

  it("keeps setDraft and closeTab working after restore and continues the query id sequence", () => {
    const restored = restoreSession(
      JSON.stringify({
        tabs: [snapTab({ id: "query-2", title: "查询 2", draft: "select 2" }), snapTab({ id: "query-5", title: "查询 5" })],
        activeId: "query-5",
      }),
    );
    expect(restored).not.toBeNull();

    const session = createSession("", restored);
    expect(session.tabs.value.map((tab) => tab.id)).toEqual(["query-2", "query-5"]);
    expect(session.activeId.value).toBe("query-5");

    session.setDraft("query-2", "select updated");
    expect(session.tabs.value[0]?.draft).toBe("select updated");

    // 新标签从恢复的最大 query-N 之后续号，不与恢复标签撞 id
    session.openQuery();
    expect(session.tabs.value.map((tab) => tab.id)).toEqual(["query-2", "query-5", "query-6"]);

    session.closeTab("query-2");
    expect(session.tabs.value.map((tab) => tab.id)).toEqual(["query-5", "query-6"]);
    expect(session.activeId.value).toBe("query-6");
  });

  it("falls back to the last tab when the active id is not in the snapshot", () => {
    const restored = restoreSession(
      JSON.stringify({
        tabs: [snapTab({ id: "table:shop:shop:public:customers", kind: "table", title: "customers", draft: "" }), snapTab({ id: "query-2", title: "查询 2", draft: "select 2" })],
        activeId: "gone",
      }),
    );
    expect(restored).not.toBeNull();
    expect(restored!.activeId).toBe("query-2");
  });

  it("starts fresh when the snapshot has no tabs left", () => {
    const restored = restoreSession('{"tabs":[],"activeId":null}');
    expect(restored).toEqual({ tabs: [], activeId: null });
    const session = createSession("", restored);
    expect(session.tabs.value).toHaveLength(0);
    expect(session.activeId.value).toBeNull();
    session.openQuery();
    expect(session.tabs.value.map((tab) => tab.id)).toEqual(["query-1"]);
  });

  it("does not persist query results or lastExecutedSql for non-query tabs", () => {
    const session = createSession();
    session.openQuery();
    session.setExecuted(session.tabs.value[0]!.id, "update t set x = 1");
    session.openTable({ id: "shop:shop:public:customers", label: "customers" });
    const table = session.tabs.value.find((tab) => tab.kind === "table")!;
    table.lastExecutedSql = "should not persist";
    table.draft = "cell edits are not persisted either";

    const snapshot = snapshotSession(session.tabs.value, table.id);
    expect(snapshot.tabs.find((tab) => tab.id === session.tabs.value[0]!.id)?.lastExecutedSql).toBe("update t set x = 1");
    expect(snapshot.tabs.find((tab) => tab.kind === "table")).toMatchObject({ lastExecutedSql: null });
  });
});
