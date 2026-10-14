import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import { catalogFixture } from "../catalog/fixtures";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import { createSession } from "../shell/session";
import HistoryPage from "./HistoryPage.vue";
import { filterHistory, openPlan, type HistoryEntry } from "./plan";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke,
}));

function entry(patch: Partial<HistoryEntry>): HistoryEntry {
  return {
    id: "h1",
    sql: "DELETE FROM customers WHERE id = 1",
    connectionId: "gone",
    connectionName: "旧连接",
    databaseName: "app",
    engine: "postgres",
    startedAt: "2026-10-14T00:00:00Z",
    finishedAt: "2026-10-14T00:00:01Z",
    status: "ok",
    elapsedMs: 12,
    summary: "1 条语句完成",
    ...patch,
  };
}

afterEach(() => {
  resetCatalog();
  invoke.mockReset();
});

describe("query history", () => {
  it("reloads a draft without executing and asks for a new target when the connection is gone", async () => {
    replaceCatalog(catalogFixture());
    const rows = [entry({}), entry({ id: "h2", sql: "SELECT 1", connectionId: "shop", connectionName: "Shop", databaseName: "shop_staging", engine: "mysql" })];
    invoke.mockImplementation(async (command: string) => {
      if (command === "query_history") return rows;
      if (command === "query_history_clear") return null;
      throw new Error(command);
    });
    const known = new Set(catalogFixture().map((item) => item.id));
    const deleted = openPlan(rows[0]!, known);
    expect(deleted.execute).toBe(false);
    expect(deleted.needsTarget).toBe(true);
    expect(deleted.connectionId).toBeNull();
    const kept = openPlan(rows[1]!, known);
    expect(kept.execute).toBe(false);
    expect(kept.needsTarget).toBe(false);
    expect(kept.connectionId).toBe("shop");
    expect(filterHistory(rows, { text: "delete", connectionId: "all", status: "all" }, known).map((item) => item.id)).toEqual(["h1"]);
    expect(filterHistory(rows, { text: "", connectionId: "deleted", status: "all" }, known).map((item) => item.id)).toEqual(["h1"]);

    const wrapper = mount(HistoryPage);
    await flushPromises();
    expect(wrapper.text()).toContain("不会执行");
    const buttons = wrapper.findAll("[data-action='open-history']");
    expect(buttons).toHaveLength(2);
    await buttons[0]!.trigger("click");
    const opened = wrapper.emitted("open")?.[0]?.[0] as { execute: boolean; needsTarget: boolean; sql: string };
    expect(opened.execute).toBe(false);
    expect(opened.needsTarget).toBe(true);
    expect(opened.sql).toContain("DELETE");
    expect(invoke.mock.calls.map((call) => call[0])).toEqual(["query_history"]);

    const session = createSession();
    const tabId = session.openQueryDraft({
      sql: opened.sql,
      connectionId: null,
      connectionName: null,
      database: null,
      engine: null,
      status: null,
      needsTarget: true,
      notice: "原连接已删除，请重新选择执行目标。暂不选择。",
    });
    const tab = session.tabs.value.find((item) => item.id === tabId);
    expect(tab?.draft).toContain("DELETE");
    expect(tab?.connectionId).toBeNull();
    expect(tab?.targetText).toBeNull();
    expect(tab?.targetNotice).toContain("暂不选择");

    await wrapper.get("[data-action='clear-history']").trigger("click");
    await flushPromises();
    expect(invoke.mock.calls.map((call) => call[0])).toContain("query_history_clear");
    expect(wrapper.text()).toContain("历史为空");
    wrapper.unmount();
  });
});
