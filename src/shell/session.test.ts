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

  it("keeps only query tabs in the snapshot", () => {
    const session = createSession();
    session.openQuery();
    session.openTable({ id: "shop:customers", label: "customers" });
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
      signature: "",
      relation: "",
    });

    const snapshot = snapshotSession(session.tabs.value, session.activeId.value);
    expect(snapshot.tabs.map((tab) => tab.id)).toEqual(["query-1"]);
    // 活动标签是被剔除的非查询标签时，回落到最后一个查询标签
    expect(snapshot.activeId).toBe("query-1");
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

  it("starts fresh when the snapshot has no query tabs left", () => {
    const restored = restoreSession('{"tabs":[],"activeId":null}');
    expect(restored).toEqual({ tabs: [], activeId: null });
    const session = createSession("", restored);
    expect(session.tabs.value).toHaveLength(0);
    expect(session.activeId.value).toBeNull();
    session.openQuery();
    expect(session.tabs.value.map((tab) => tab.id)).toEqual(["query-1"]);
  });
});
