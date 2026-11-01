import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { CatalogExtras } from "../catalog/types";
import type { StructurePlan, StructureReceipt } from "../structure/types";
import DatabasePanel from "./DatabasePanel.vue";
import { adminSections, sequenceAction, sequenceDraft } from "./objects";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({ invoke }));

afterEach(() => {
  invoke.mockReset();
});

function emptyExtras(): CatalogExtras {
  return {
    extensions: { items: [], error: null },
    sequences: { items: [], error: null },
    events: { items: [], error: null },
    partitions: { items: [], error: null },
    pragmas: { journalMode: null, userVersion: null, foreignKeys: null, error: null },
    attached: { items: [], error: null },
  };
}

function plan(overrides: Partial<StructurePlan> = {}): StructurePlan {
  return {
    blocked: [],
    risks: [{ level: "info", text: "说明" }],
    steps: [{ sql: "ALTER SEQUENCE public.orders_seq INCREMENT BY 5;", label: "修改序列" }],
    rule: "逐条执行",
    rebuild: false,
    target: "postgres / rillsql / public",
    ...overrides,
  };
}

function receipt(overrides: Partial<StructureReceipt> = {}): StructureReceipt {
  return {
    blocked: [],
    rule: "逐条执行",
    target: "postgres / rillsql / public",
    rebuild: false,
    steps: [{ sql: "ALTER SEQUENCE public.orders_seq INCREMENT BY 5;", label: "修改序列", status: "ok", message: "" }],
    ...overrides,
  };
}

/** 让 catalog_refresh 返回带 extras 的目录，ddl_* 返回给定的预览与回执。 */
function mockBackend(extras: CatalogExtras, preview: StructurePlan = plan(), result: StructureReceipt = receipt()) {
  invoke.mockImplementation((command: string) => {
    if (command === "catalog_refresh") return Promise.resolve([{ id: "conn", extras }]);
    if (command === "ddl_preview") return Promise.resolve(preview);
    if (command === "ddl_execute") return Promise.resolve(result);
    return Promise.resolve(null);
  });
}

function previewCalls(): unknown[] {
  return invoke.mock.calls.filter(([command]) => command === "ddl_preview").map(([, args]) => args);
}

function executeCalls(): unknown[] {
  return invoke.mock.calls.filter(([command]) => command === "ddl_execute").map(([, args]) => args);
}

function buttonByText(wrapper: VueWrapper, text: string): HTMLButtonElement | null {
  const found = wrapper.findAll("button").find((node) => node.text().trim() === text);
  return found ? (found.element as HTMLButtonElement) : null;
}

async function mountPanel(engine: "sqlite" | "postgres" | "mysql"): Promise<VueWrapper> {
  const wrapper = mount(DatabasePanel, {
    props: {
      connectionId: "conn",
      connectionName: "127.0.0.1",
      database: "rillsql",
      engine,
      schema: engine === "postgres" ? "public" : "",
    },
    attachTo: document.body,
  });
  await flushPromises();
  return wrapper;
}

describe("admin section routing", () => {
  it("only gives SQLite the pragma, attached and maintenance sections", () => {
    expect(adminSections("sqlite")).toMatchObject({ pragma: true, attached: true, maintenance: true, extensions: false, events: false });
    expect(adminSections("postgres")).toMatchObject({ extensions: true, sequences: true, pragma: false, events: false });
    expect(adminSections("mysql")).toMatchObject({ events: true, partitions: true, extensions: false });
    expect(adminSections("mariadb")).toMatchObject({ events: true, partitions: true });
    expect(adminSections("sqlserver")).toMatchObject({ pragma: false, extensions: false, events: false, partitions: false });
  });

  it("turns only changed sequence parameters into an ALTER SEQUENCE action", () => {
    const sequence = {
      schema: "public",
      name: "orders_seq",
      dataType: "bigint",
      startValue: 1,
      minValue: 1,
      maxValue: 1000,
      increment: 1,
      cacheSize: 1,
      cycle: false,
      lastValue: 42,
    };
    const draft = sequenceDraft(sequence);
    expect(sequenceAction(sequence, draft)).toBeNull();
    const change = sequenceAction(sequence, { ...draft, increment: "5" });
    expect(change).toEqual({
      schema: "public",
      action: { kind: "alterSequence", sequence: "orders_seq", increment: 5, minValue: null, maxValue: null, start: null, cycle: null },
    });
    expect(sequenceAction(sequence, { ...draft, cycle: true })?.action).toMatchObject({ cycle: true, increment: null });
    // 非法文本按“没有改”处理，不生成语句。
    expect(sequenceAction(sequence, { ...draft, increment: "abc" })).toBeNull();
  });
});

describe("database panel", () => {
  it("shows SQLite PRAGMA and attached databases, and hides PostgreSQL sections", async () => {
    const extras = emptyExtras();
    extras.pragmas = { journalMode: "wal", userVersion: 3, foreignKeys: true, error: null };
    extras.attached.items = [{ name: "aux", file: "/tmp/aux.db" }];
    mockBackend(extras);
    const wrapper = await mountPanel("sqlite");

    expect(wrapper.find("[data-admin-section='pragma']").exists()).toBe(true);
    expect(wrapper.find("[data-admin-section='attached']").exists()).toBe(true);
    expect(wrapper.find("[data-admin-section='maintenance']").exists()).toBe(true);
    expect(wrapper.find("[data-admin-section='extensions']").exists()).toBe(false);
    expect(wrapper.find("[data-admin-section='events']").exists()).toBe(false);
    expect(wrapper.get("[data-foreign-keys-state]").text()).toBe("开启");
    expect(wrapper.get("[data-attached='aux']").text()).toContain("/tmp/aux.db");
    wrapper.unmount();
  });

  it("previews foreign key changes before anything is executed", async () => {
    const extras = emptyExtras();
    extras.pragmas = { journalMode: "delete", userVersion: 0, foreignKeys: true, error: null };
    mockBackend(extras);
    const wrapper = await mountPanel("sqlite");

    await wrapper.get("[data-action='toggle-foreign-keys']").trigger("click");
    await flushPromises();

    expect(previewCalls()).toHaveLength(1);
    expect((previewCalls()[0] as { request: { action: unknown } }).request.action).toEqual({
      kind: "setForeignKeys",
      enabled: false,
    });
    expect(executeCalls()).toHaveLength(0);
    // 预览给的是确认框，未确认前不执行
    expect(buttonByText(wrapper, "确认执行")).toBeTruthy();
    wrapper.unmount();
  });

  it("hides the execute entry when the plan is blocked", async () => {
    const extras = emptyExtras();
    extras.pragmas = { journalMode: "delete", userVersion: 0, foreignKeys: false, error: null };
    mockBackend(extras, plan({ blocked: ["这个引擎不支持"], steps: [] }));
    const wrapper = await mountPanel("sqlite");

    await wrapper.get("[data-action='toggle-foreign-keys']").trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("这个引擎不支持");
    // 挡住时不给出可用的执行入口：确认按钮禁用，执行通道不会被调用
    expect(buttonByText(wrapper, "确认执行")?.disabled).toBe(true);
    expect(executeCalls()).toHaveLength(0);
    wrapper.unmount();
  });

  it("executes the confirmed statements and reports the receipt", async () => {
    const extras = emptyExtras();
    extras.pragmas = { journalMode: "delete", userVersion: 0, foreignKeys: false, error: null };
    mockBackend(extras);
    const wrapper = await mountPanel("sqlite");

    await wrapper.get("[data-action='vacuum']").trigger("click");
    await flushPromises();
    expect((previewCalls()[0] as { request: { action: unknown } }).request.action).toEqual({ kind: "vacuum" });

    buttonByText(wrapper, "确认执行")!.click();
    await flushPromises();

    expect(executeCalls()).toHaveLength(1);
    expect((executeCalls()[0] as { request: { confirmedSql: string[] } }).request.confirmedSql).toEqual([
      "ALTER SEQUENCE public.orders_seq INCREMENT BY 5;",
    ]);
    expect(wrapper.text()).toContain("执行结果");
    wrapper.unmount();
  });

  it("attaches a database only after the preview is confirmed", async () => {
    const extras = emptyExtras();
    extras.pragmas = { journalMode: "delete", userVersion: 0, foreignKeys: true, error: null };
    mockBackend(extras, plan({ steps: [{ sql: "ATTACH DATABASE '/tmp/aux.db' AS aux;", label: "附加 aux" }] }));
    const wrapper = await mountPanel("sqlite");

    await wrapper.get("[data-field='attach-path']").setValue("/tmp/aux.db");
    await wrapper.get("[data-field='attach-name']").setValue("aux");
    await wrapper.get("[data-action='attach-database']").trigger("click");
    await flushPromises();

    expect((previewCalls()[0] as { request: { action: unknown } }).request.action).toEqual({
      kind: "attachDatabase",
      path: "/tmp/aux.db",
      name: "aux",
    });
    wrapper.unmount();
  });

  it("lists PostgreSQL extensions and previews a sequence change with its own schema", async () => {
    const extras = emptyExtras();
    extras.extensions.items = [
      { name: "pg_trgm", defaultVersion: "1.6", installedVersion: "1.6", comment: "trigram" },
      { name: "citext", defaultVersion: "1.6", installedVersion: null, comment: "" },
    ];
    extras.sequences.items = [
      {
        schema: "audit",
        name: "log_seq",
        dataType: "bigint",
        startValue: 10,
        minValue: 1,
        maxValue: 1000,
        increment: 1,
        cacheSize: 1,
        cycle: false,
        lastValue: 12,
      },
    ];
    mockBackend(extras);
    const wrapper = await mountPanel("postgres");

    expect(wrapper.find("[data-admin-section='extensions']").exists()).toBe(true);
    expect(wrapper.get("[data-extension='pg_trgm']").text()).toContain("已安装");
    expect(wrapper.get("[data-extension='citext']").text()).toContain("未安装");

    await wrapper.get("[data-action='edit-sequence']").trigger("click");
    await wrapper.get("[data-field='sequence-increment']").setValue("5");
    await wrapper.get("[data-action='preview-sequence']").trigger("click");
    await flushPromises();

    const call = previewCalls()[0] as { request: { schema: string; action: unknown } };
    expect(call.request.schema).toBe("audit");
    expect(call.request.action).toEqual({
      kind: "alterSequence",
      sequence: "log_seq",
      increment: 5,
      minValue: null,
      maxValue: null,
      start: null,
      cycle: null,
    });
    wrapper.unmount();
  });

  it("lists MySQL events and partitions and toggles an event through the preview", async () => {
    const extras = emptyExtras();
    extras.events.items = [
      {
        database: "shop",
        name: "cleanup",
        status: "ENABLED",
        schedule: "EVERY 1 DAY",
        onCompletion: "PRESERVE",
        definer: "rillsql@%",
        lastExecuted: "2026-10-31 10:00:00",
        definition: "UPDATE orders SET stale = 0",
      },
    ];
    extras.partitions.items = [
      {
        database: "shop",
        table: "orders",
        method: "RANGE",
        expression: "YEAR(created_at)",
        partitions: [{ name: "p2025", description: "2025", rows: 12 }],
      },
    ];
    mockBackend(extras, plan({ steps: [{ sql: "ALTER EVENT shop.cleanup DISABLE;", label: "停用事件" }] }));
    const wrapper = await mountPanel("mysql");

    expect(wrapper.get("[data-event='cleanup']").text()).toContain("ENABLED");
    expect(wrapper.get("[data-partition-table='orders']").text()).toContain("RANGE");
    expect(wrapper.get("[data-partition-table='orders']").text()).toContain("p2025");

    // 事件体只读：展开就能看 DDL，写操作仍然只有启停。
    expect(wrapper.find("[data-event-definition]").exists()).toBe(false);
    await wrapper.get("[data-action='toggle-event-ddl']").trigger("click");
    await flushPromises();
    expect(wrapper.get("[data-event-definition]").text()).toBe("UPDATE orders SET stale = 0");
    expect(previewCalls()).toEqual([]);

    await wrapper.get("[data-action='disable-event']").trigger("click");
    await flushPromises();
    expect((previewCalls()[0] as { request: { action: unknown } }).request.action).toEqual({
      kind: "setEventEnabled",
      event: "cleanup",
      enabled: false,
    });
    wrapper.unmount();
  });

  it("keeps a failed section visible without hiding the others", async () => {
    const extras = emptyExtras();
    extras.extensions.error = "权限不足";
    extras.sequences.items = [
      {
        schema: "public",
        name: "orders_seq",
        dataType: "bigint",
        startValue: 1,
        minValue: 1,
        maxValue: 100,
        increment: 1,
        cacheSize: 1,
        cycle: false,
        lastValue: null,
      },
    ];
    mockBackend(extras);
    const wrapper = await mountPanel("postgres");

    expect(wrapper.get("[data-admin-section='extensions']").text()).toContain("权限不足");
    expect(wrapper.find("[data-sequence='orders_seq']").exists()).toBe(true);
    wrapper.unmount();
  });
});
