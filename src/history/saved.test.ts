import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import { catalogFixture } from "../catalog/fixtures";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import HistoryPage from "./HistoryPage.vue";
import { deletedConnectionNotice, openPlan, type HistoryEntry } from "./plan";
import { filterSaved, openSaved, savedNameError, type SavedQuery, type SavedWrite } from "./saved";
import { createSession } from "../shell/session";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke,
}));

function saved(patch: Partial<SavedQuery> = {}): SavedQuery {
  return {
    id: "s1",
    name: "日报",
    sql: "SELECT id, amount FROM orders",
    connectionId: "shop",
    connectionName: "Shop",
    databaseName: "shop_staging",
    engine: "mysql",
    updatedAt: "2026-10-15T00:00:00Z",
    ...patch,
  };
}

function historyRow(): HistoryEntry {
  return {
    id: "h1",
    sql: "DELETE FROM customers WHERE id = 1",
    connectionId: "shop",
    connectionName: "Shop",
    databaseName: "shop_staging",
    engine: "mysql",
    startedAt: "2026-10-14T10:00:00",
    finishedAt: "2026-10-14T10:00:01",
    status: "ok",
    elapsedMs: 12,
    summary: "1 条语句完成",
  };
}

afterEach(() => {
  resetCatalog();
  invoke.mockReset();
});

describe("saved queries", () => {
  it("opens a named query without executing and asks for a new target when the connection is gone", () => {
    const known = new Set(catalogFixture().map((item) => item.id));
    const kept = openSaved(saved(), known);
    expect(kept.execute).toBe(false);
    expect(kept.needsTarget).toBe(false);
    expect(kept.sql).toBe("SELECT id, amount FROM orders");
    expect(kept.connectionId).toBe("shop");
    expect(kept.database).toBe("shop_staging");

    const gone = openSaved(saved({ connectionId: "gone", connectionName: "旧连接", databaseName: "app" }), known);
    expect(gone.execute).toBe(false);
    expect(gone.needsTarget).toBe(true);
    expect(gone.connectionId).toBeNull();
    expect(gone.database).toBeNull();
    expect(gone.sql).toBe("SELECT id, amount FROM orders");
    expect(deletedConnectionNotice()).toContain("需要重新选择执行目标");
    expect(deletedConnectionNotice()).toContain("不会执行");
    expect(filterSaved([saved(), saved({ id: "s2", name: "周报", sql: "SELECT 2" })], "日报").map((item) => item.id)).toEqual(["s1"]);
    expect(savedNameError("  ")).toBe("请填写名称");

    const session = createSession();
    session.openQueryDraft({
      sql: gone.sql,
      connectionId: null,
      connectionName: gone.connectionName,
      database: null,
      engine: gone.engine,
      status: null,
      needsTarget: true,
      notice: deletedConnectionNotice(),
    });
    expect(session.activeTab.value?.draft).toBe("SELECT id, amount FROM orders");
    expect(session.activeTab.value?.targetNotice).toContain("需要重新选择执行目标");
    expect(session.tabs.value).toHaveLength(1);
  });

  it("saves the current SQL under a name and opens it without writing execution history", async () => {
    replaceCatalog(catalogFixture());
    const rows: SavedQuery[] = [];
    invoke.mockImplementation(async (command: string, args?: { request?: { name: string; sql: string; connectionId: string; connectionName: string; databaseName: string; engine: string } }) => {
      if (command === "query_history") return [historyRow()];
      if (command === "query_saved_list") return rows.map((item) => ({ ...item }));
      if (command === "query_saved_save") {
        const request = args?.request;
        if (!request) throw new Error("missing request");
        const query = saved({ name: request.name.trim(), sql: request.sql, connectionId: request.connectionId, connectionName: request.connectionName, databaseName: request.databaseName, engine: request.engine });
        rows.splice(0, rows.length, query);
        const written: SavedWrite = { query, created: true };
        return written;
      }
      throw new Error(command);
    });

    const wrapper = mount(HistoryPage, {
      props: {
        section: "saved",
        currentDraft: { sql: "SELECT id, amount FROM orders", connectionId: "shop", connectionName: "Shop", databaseName: "shop_staging", engine: "mysql" },
      },
    });
    await flushPromises();
    expect(wrapper.text()).toContain("SELECT id, amount FROM orders");
    expect(wrapper.text()).not.toContain("DELETE FROM customers");
    await wrapper.get("form").trigger("submit");
    await flushPromises();
    expect(wrapper.get(".saved-alert").text()).toContain("请填写名称");
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("query_saved_save");

    await wrapper.get("[aria-label='保存查询的名称']").setValue("日报");
    await wrapper.get("form").trigger("submit");
    await flushPromises();
    expect(wrapper.text()).toContain("已保存「日报」");
    expect(wrapper.text()).toContain("没有写入执行历史");
    expect(wrapper.text()).toContain("日报");
    const saveCall = invoke.mock.calls.find((call) => call[0] === "query_saved_save");
    expect(saveCall?.[1]).toMatchObject({ request: { name: "日报", sql: "SELECT id, amount FROM orders", connectionId: "shop" } });

    const beforeOpen = invoke.mock.calls.map((call) => call[0]);
    await wrapper.get("[data-action='open-saved']").trigger("click");
    const opened = wrapper.emitted("open")?.at(-1)?.[0] as { execute: boolean; needsTarget: boolean; sql: string; connectionId: string | null };
    expect(opened.execute).toBe(false);
    expect(opened.needsTarget).toBe(false);
    expect(opened.sql).toBe("SELECT id, amount FROM orders");
    expect(opened.connectionId).toBe("shop");
    expect(invoke.mock.calls.map((call) => call[0])).toEqual(beforeOpen);
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("query_start");
    wrapper.unmount();
  });

  it("keeps the SQL when the saved connection is gone and does not execute", async () => {
    replaceCatalog(catalogFixture());
    const row = saved({ connectionId: "gone", connectionName: "旧连接", databaseName: "app", engine: "postgres" });
    invoke.mockImplementation(async (command: string) => {
      if (command === "query_history") return [];
      if (command === "query_saved_list") return [row];
      throw new Error(command);
    });
    const wrapper = mount(HistoryPage, { props: { section: "saved", currentDraft: null } });
    await flushPromises();
    expect(wrapper.text()).toContain("需要重新选择执行目标");
    expect(wrapper.text()).toContain("SELECT id, amount FROM orders");
    expect(wrapper.get("[data-action='save-named-query']").attributes("disabled")).toBeDefined();
    await wrapper.get("[data-action='open-saved']").trigger("click");
    const opened = wrapper.emitted("open")?.[0]?.[0] as { execute: boolean; needsTarget: boolean; sql: string; connectionId: string | null };
    expect(opened.execute).toBe(false);
    expect(opened.needsTarget).toBe(true);
    expect(opened.sql).toBe(row.sql);
    expect(opened.connectionId).toBeNull();
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("query_start");
    wrapper.unmount();
  });

  it("leaves the query in the list when deletion is cancelled and removes it after confirmation", async () => {
    replaceCatalog(catalogFixture());
    const rows = [saved(), saved({ id: "s2", name: "周报", sql: "SELECT 2" })];
    invoke.mockImplementation(async (command: string, args?: { id?: string; request?: { id: string; name: string } }) => {
      if (command === "query_history") return [historyRow()];
      if (command === "query_saved_list") return rows.map((item) => ({ ...item }));
      if (command === "query_saved_rename") {
        const request = args?.request;
        if (!request) throw new Error("missing request");
        const row = rows.find((item) => item.id === request.id);
        if (!row) throw new Error("找不到这个保存查询");
        row.name = request.name;
        return { ...row };
      }
      if (command === "query_saved_delete") {
        const index = rows.findIndex((item) => item.id === args?.id);
        if (index < 0) throw new Error("找不到这个保存查询");
        rows.splice(index, 1);
        return null;
      }
      throw new Error(command);
    });
    const wrapper = mount(HistoryPage, { props: { section: "saved" } });
    await flushPromises();
    expect(wrapper.text()).toContain("日报");
    expect(wrapper.text()).toContain("周报");
    await wrapper.get("[aria-label='按名称查找']").setValue("周");
    expect(wrapper.text()).toContain("周报");
    expect(wrapper.text()).not.toContain("日报");
    await wrapper.get("[aria-label='按名称查找']").setValue("");

    const dailyDelete = wrapper.findAll("[data-action='delete-saved']")[0]!;
    await dailyDelete.trigger("click");
    expect(wrapper.text()).toContain("日报");
    await wrapper.get("[data-action='cancel-delete-saved']").trigger("click");
    expect(wrapper.text()).toContain("日报");
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("query_saved_delete");

    await wrapper.findAll("[data-action='delete-saved']")[0]!.trigger("click");
    await wrapper.get("[data-action='confirm-delete-saved']").trigger("click");
    await flushPromises();
    expect(wrapper.text()).not.toContain("日报");
    expect(wrapper.text()).toContain("周报");
    expect(invoke.mock.calls.map((call) => call[0])).toContain("query_saved_delete");

    await wrapper.get("[data-action='rename-saved']").trigger("click");
    await wrapper.get("[aria-label='重命名 周报']").setValue("晨报");
    await wrapper.get("[data-action='cancel-rename-saved']").trigger("click");
    expect(wrapper.text()).toContain("周报");
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("query_saved_rename");
    await wrapper.get("[data-action='rename-saved']").trigger("click");
    await wrapper.get("[aria-label='重命名 周报']").setValue("晨报");
    await wrapper.get("form.saved-rename").trigger("submit");
    await flushPromises();
    expect(wrapper.text()).toContain("晨报");
    expect(wrapper.text()).not.toContain("周报");
    wrapper.unmount();
  });

  it("keeps execution history loadable and does not replace it with saved queries", async () => {
    replaceCatalog(catalogFixture());
    invoke.mockImplementation(async (command: string) => {
      if (command === "query_history") return [historyRow()];
      if (command === "query_saved_list") return [saved()];
      throw new Error(command);
    });
    const wrapper = mount(HistoryPage);
    await flushPromises();
    expect(wrapper.text()).toContain("DELETE FROM customers WHERE id = 1");
    expect(wrapper.text()).not.toContain("SELECT id, amount FROM orders");
    expect(wrapper.text()).toContain("不会执行");
    await wrapper.get("[data-action='open-history']").trigger("click");
    const historyOpened = wrapper.emitted("open")?.[0]?.[0] as { execute: boolean; sql: string };
    expect(historyOpened.execute).toBe(false);
    expect(historyOpened.sql).toContain("DELETE");
    expect(openPlan(historyRow(), new Set(["shop"])).execute).toBe(false);

    await wrapper.get("[data-action='show-saved']").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("日报");
    expect(wrapper.text()).toContain("SELECT id, amount FROM orders");
    expect(wrapper.text()).not.toContain("DELETE FROM customers");
    await wrapper.get("[data-action='show-history']").trigger("click");
    expect(wrapper.text()).toContain("DELETE FROM customers WHERE id = 1");
    expect(wrapper.text()).not.toContain("SELECT id, amount FROM orders");
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("query_start");
    wrapper.unmount();
  });
});
