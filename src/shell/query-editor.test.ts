import { afterEach, describe, expect, it, vi } from "vitest";
import {
  buildInsertText,
  FOCUS_GRACE_MS,
  focusedEditor,
  insertAtCursor,
  insertPlan,
  registerEditor,
  resetEditors,
  setEditorFocus,
  setEditorReadOnly,
  unregisterEditor,
  type InsertReference,
  type QueryEditorApi,
} from "./query-editor";

function reference(patch: Partial<InsertReference> = {}): InsertReference {
  return {
    connectionId: "atlas",
    connectionName: "Atlas",
    engine: "postgres",
    database: "atlas_dev",
    schema: "public",
    table: "orders",
    column: "status",
    ...patch,
  };
}

function fakeEditor(patch: Partial<QueryEditorApi> = {}): QueryEditorApi & { inserted: InsertReference[] } {
  const inserted: InsertReference[] = [];
  return {
    inserted,
    insert: (item) => {
      inserted.push(item);
      return true;
    },
    focus: () => undefined,
    isRunning: () => false,
    ...patch,
  };
}

afterEach(() => {
  resetEditors();
  vi.useRealTimers();
});

describe("tree click insertion text", () => {
  it("uses the statement alias, then the table name, then the bare column", () => {
    const alias = "SELECT * FROM orders o WHERE ";
    expect(buildInsertText(reference(), alias, alias.length)).toBe("o.status");

    const plain = "SELECT * FROM orders WHERE ";
    expect(buildInsertText(reference(), plain, plain.length)).toBe("orders.status");

    const unrelated = "SELECT ";
    expect(buildInsertText(reference(), unrelated, unrelated.length)).toBe("status");
  });

  it("only reads the statement the cursor sits in", () => {
    const draft = "SELECT * FROM orders o WHERE ;\nSELECT ";
    const cursor = draft.length;
    expect(buildInsertText(reference(), draft, cursor)).toBe("status");
  });

  it("qualifies table names by engine, schema and current database", () => {
    expect(buildInsertText(reference({ column: null }), "", 0)).toBe("orders");
    expect(buildInsertText(reference({ column: null, schema: "analytics" }), "", 0)).toBe("analytics.orders");
    expect(
      buildInsertText(reference({ column: null, engine: "mysql", database: "shop_staging", schema: "" }), "", 0, "shop_staging"),
    ).toBe("orders");
    expect(
      buildInsertText(reference({ column: null, engine: "mysql", database: "shop_staging", schema: "" }), "", 0, "shop_prod"),
    ).toBe("shop_staging.orders");
    expect(buildInsertText(reference({ column: null, engine: "sqlite", database: "main", schema: "main" }), "", 0)).toBe("orders");
    expect(buildInsertText(reference({ column: null, engine: "sqlite", database: "main", schema: "aux" }), "", 0)).toBe("aux.orders");
  });

  it("quotes identifiers that need it and keeps the original case", () => {
    expect(
      buildInsertText(
        reference({ column: null, engine: "mysql", database: "shop_staging", schema: "", table: "Order Items" }),
        "",
        0,
        "shop_staging",
      ),
    ).toBe("\u0060Order Items\u0060");
    expect(buildInsertText(reference({ column: "order qty" }), "", 0)).toBe('"order qty"');
    const sized = "SELECT * FROM Orders WHERE ";
    expect(buildInsertText(reference({ table: "Orders" }), sized, sized.length)).toBe("Orders.status");
  });

  it("matches the statement table case-insensitively", () => {
    const draft = "select * from ORDERS o where ";
    expect(buildInsertText(reference(), draft, draft.length)).toBe("o.status");
  });
});

describe("query editor registry", () => {
  it("tracks the focused editor and degrades after the grace window", () => {
    const api = fakeEditor();
    registerEditor("query-1", api);
    setEditorFocus("query-1", true);
    expect(focusedEditor()?.tabId).toBe("query-1");

    setEditorFocus("query-1", false);
    const blurredAt = Date.now();
    expect(focusedEditor(blurredAt)?.tabId).toBe("query-1");
    expect(focusedEditor(blurredAt + FOCUS_GRACE_MS + 1)).toBeNull();

    unregisterEditor("query-1");
    expect(focusedEditor()).toBeNull();
  });

  it("inserts into the registered editor and reports guards without touching it", () => {
    const api = fakeEditor();
    registerEditor("query-1", api);
    expect(insertAtCursor("query-1", reference())).toBe("ok");
    expect(api.inserted).toHaveLength(1);

    setEditorReadOnly("query-1", true);
    expect(insertAtCursor("query-1", reference())).toBe("readonly");
    setEditorReadOnly("query-1", false);

    const busy = fakeEditor({ isRunning: () => true });
    registerEditor("query-2", busy);
    expect(insertAtCursor("query-2", reference())).toBe("running");
    expect(busy.inserted).toHaveLength(0);

    expect(insertAtCursor("query-missing", reference())).toBe("none");
  });

  it("drops the registration when a tab unmounts", () => {
    const api = fakeEditor();
    registerEditor("query-1", api);
    setEditorFocus("query-1", true);
    unregisterEditor("query-1");
    expect(insertAtCursor("query-1", reference())).toBe("none");
    expect(focusedEditor()).toBeNull();
  });

  it("plans insert, select and create in that order", () => {
    expect(insertPlan(true, true)).toBe("insert");
    expect(insertPlan(true, false)).toBe("insert");
    expect(insertPlan(false, true)).toBe("select");
    expect(insertPlan(false, false)).toBe("create");
  });
});
