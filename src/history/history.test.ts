import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import { catalogFixture } from "../catalog/fixtures";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import HistoryPage from "./HistoryPage.vue";
import { filterHistory, groupHistoryByDay, openPlan, statusLabel, type HistoryEntry } from "./plan";

const pageSource = readFileSync(resolve(dirname(fileURLToPath(import.meta.url)), "HistoryPage.vue"), "utf8");

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
    startedAt: "2026-10-14T10:00:00",
    finishedAt: "2026-10-14T10:00:01",
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

async function pick(wrapper: ReturnType<typeof mount>, label: string, option: string) {
  await wrapper.get(`[role="combobox"][aria-label="${label}"]`).trigger("click");
  const match = wrapper.findAll('[role="option"]').find((node) => node.text() === option);
  expect(match, option).toBeTruthy();
  await match!.trigger("click");
}

describe("query history", () => {
  it("plans a re-open that never executes and asks for a new target when the connection is gone", () => {
    const rows = [entry({}), entry({ id: "h2", sql: "SELECT 1", connectionId: "shop", connectionName: "Shop", databaseName: "shop_staging", engine: "mysql" })];
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
  });

  it("groups entries by day with plain status labels", () => {
    const rows = [
      entry({ id: "h1", startedAt: "2026-10-14T10:00:00", status: "ok" }),
      entry({ id: "h2", sql: "SELECT 1", startedAt: "2026-10-14T18:30:00", status: "error", summary: "语法错误" }),
      entry({ id: "h3", sql: "SELECT 2", startedAt: "2026-10-13T09:00:00", status: "cancelled", summary: "已取消" }),
    ];
    const groups = groupHistoryByDay(rows, new Date(2026, 9, 20, 12, 0));
    expect(groups.map((group) => group.label)).toEqual(["2026 年 10 月 14 日", "2026 年 10 月 13 日"]);
    expect(groups[0]!.entries.map((item) => item.id)).toEqual(["h1", "h2"]);
    expect(groups[1]!.entries.map((item) => item.id)).toEqual(["h3"]);
    expect(statusLabel("ok")).toBe("成功");
    expect(statusLabel("error")).toBe("失败");
    expect(statusLabel("cancelled")).toBe("已取消");
    expect(statusLabel("unknown")).toBe("结果未知");
    expect(groupHistoryByDay(rows, new Date(2026, 9, 14, 23, 0))[0]!.label).toBe("今天");
  });

  it("renders grouped history, reloads a draft without executing and marks deleted connections", async () => {
    replaceCatalog(catalogFixture());
    const rows = [
      entry({}),
      entry({ id: "h2", sql: "SELECT 1", connectionId: "shop", connectionName: "Shop", databaseName: "shop_staging", engine: "mysql", startedAt: "2026-10-13T09:00:00", status: "error", summary: "1 条语句失败" }),
    ];
    invoke.mockImplementation(async (command: string) => {
      if (command === "query_history") return rows;
      if (command === "query_history_clear") return null;
      throw new Error(command);
    });
    const wrapper = mount(HistoryPage);
    await flushPromises();
    expect(wrapper.text()).toContain("不会执行");
    expect(wrapper.findAll(".hist-day h2").map((node) => node.text())).toEqual(groupHistoryByDay(rows).map((group) => group.label));
    expect(wrapper.text()).toContain("成功");
    expect(wrapper.text()).toContain("失败");
    expect(wrapper.text()).toContain("原连接已删除");
    expect(wrapper.text()).toContain("载入后重新选择连接和库");

    expect(wrapper.find("select").exists()).toBe(false);
    expect(wrapper.get("[data-action='show-history']").attributes("aria-selected")).toBe("true");
    expect(wrapper.get("[data-action='show-saved']").attributes("aria-selected")).toBe("false");
    expect(wrapper.findAll(".hist-switch button").every((button) => !button.classes().includes("primary"))).toBe(true);
    expect(pageSource).toContain('.hist-switch button.btn[aria-selected="true"] { background: var(--sel-strong); color: var(--text); font-weight: 600; }');
    expect(pageSource).not.toMatch(/class="[^"]*\bprimary\b[^"]*"[^>]*data-action="confirm-rename-saved"/);
    expect(wrapper.findAll("button.primary").map((button) => button.text())).toEqual(["载入编辑器"]);
    expect(wrapper.get(".history-inspector .btn.primary").text()).toContain("载入编辑器");
    expect(wrapper.findAll("[data-action='open-history']").every((button) => !button.classes().includes("primary"))).toBe(true);
    expect(wrapper.get("[data-action='clear-history']").classes()).toContain("danger");

    const buttons = wrapper.findAll("[data-action='open-history']");
    expect(buttons).toHaveLength(2);
    expect(buttons[0]!.text()).toContain("载入编辑器");
    await buttons[0]!.trigger("click");
    const opened = wrapper.emitted("open")?.[0]?.[0] as { execute: boolean; needsTarget: boolean; sql: string };
    expect(opened.execute).toBe(false);
    expect(opened.needsTarget).toBe(true);
    expect(opened.sql).toContain("DELETE");
    expect(invoke.mock.calls.map((call) => call[0])).toEqual(["query_history"]);

    await pick(wrapper, "按结果筛选", "失败");
    expect(wrapper.findAll("[data-action='open-history']")).toHaveLength(1);
    expect(wrapper.text()).not.toContain("旧连接 / app");
    await pick(wrapper, "按结果筛选", "全部结果");
    await pick(wrapper, "按连接筛选", "Shop");
    expect(wrapper.findAll("[data-action='open-history']")).toHaveLength(1);
    expect(wrapper.text()).toContain("Shop / shop_staging");
    await pick(wrapper, "按连接筛选", "全部连接");
    await wrapper.get("input[aria-label='搜索历史']").setValue("select");
    expect(wrapper.findAll("[data-action='open-history']")).toHaveLength(1);

    await wrapper.get("[data-action='clear-history']").trigger("click");
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("query_history_clear");
    await wrapper.get("[data-action='confirm-clear-history']").trigger("click");
    await flushPromises();
    expect(invoke.mock.calls.map((call) => call[0])).toContain("query_history_clear");
    expect(wrapper.text()).toContain("历史为空");
    wrapper.unmount();
  });

  it("keeps one sentence under the saved query title", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "query_history" || command === "query_saved_list") return [];
      throw new Error(command);
    });
    const wrapper = mount(HistoryPage, { props: { section: "saved" } });
    await flushPromises();
    const lead = wrapper.get(".page-head p").text();
    expect(lead.split("。").filter((part) => part.trim())).toEqual(["按名称保存当前 SQL"]);
    expect(wrapper.get(".page-head h1").text()).toBe("保存的查询");
    expect(wrapper.get("[data-action='show-saved']").attributes("aria-selected")).toBe("true");
    expect(wrapper.findAll(".hist-switch button").every((button) => !button.classes().includes("primary"))).toBe(true);
    expect(wrapper.findAll("button.primary").map((button) => button.attributes("data-action"))).toEqual(["save-named-query"]);
    wrapper.unmount();
  });

  it("offers a retry when history cannot be read", async () => {
    invoke.mockRejectedValueOnce(new Error("磁盘读不了"));
    const wrapper = mount(HistoryPage);
    await flushPromises();
    const alert = wrapper.get("[role='alert']");
    expect(alert.text()).toContain("历史没有读出来");
    expect(alert.text()).toContain("磁盘读不了");
    invoke.mockImplementation(async (command: string) => {
      if (command === "query_history") return [entry({})];
      throw new Error(command);
    });
    await wrapper.get("[data-action='retry-history']").trigger("click");
    await flushPromises();
    expect(wrapper.find("[role='alert']").exists()).toBe(false);
    expect(wrapper.findAll("[data-action='open-history']")).toHaveLength(1);
    wrapper.unmount();
  });

  it("deletes one history entry after a two-step confirm and reloads the list", async () => {
    replaceCatalog(catalogFixture());
    const rows = [
      entry({}),
      entry({ id: "h2", sql: "SELECT 1", connectionId: "shop", connectionName: "Shop", databaseName: "shop_staging", engine: "mysql", startedAt: "2026-10-13T09:00:00", status: "error", summary: "1 条语句失败" }),
    ];
    invoke.mockImplementation(async (command: string) => {
      if (command === "query_history") return rows;
      if (command === "query_history_delete") return null;
      throw new Error(command);
    });
    const wrapper = mount(HistoryPage);
    await flushPromises();
    expect(wrapper.findAll("[data-action='delete-run']")).toHaveLength(2);
    await wrapper.findAll("[data-action='delete-run']")[0]!.trigger("click");
    expect(invoke.mock.calls.map((call) => call[0])).toEqual(["query_history"]);
    expect(wrapper.text()).toContain("删除这条查询历史");
    await wrapper.get("[data-action='cancel-delete-run']").trigger("click");
    expect(wrapper.find("[data-action='confirm-delete-run']").exists()).toBe(false);
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("query_history_delete");
    await wrapper.findAll("[data-action='delete-run']")[0]!.trigger("click");
    await wrapper.get("[data-action='confirm-delete-run']").trigger("click");
    await flushPromises();
    expect(invoke.mock.calls.filter((call) => call[0] === "query_history_delete").map((call) => call[1])).toEqual([{ id: "h1" }]);
    expect(invoke.mock.calls.filter((call) => call[0] === "query_history")).toHaveLength(2);
    expect(wrapper.find("[data-action='confirm-delete-run']").exists()).toBe(false);
    wrapper.unmount();
  });

  it("keeps the entry and shows a message when deleting one history entry fails", async () => {
    replaceCatalog(catalogFixture());
    invoke.mockImplementation(async (command: string) => {
      if (command === "query_history") return [entry({})];
      if (command === "query_history_delete") throw "删不了这条查询历史：库被占用";
      throw new Error(command);
    });
    const wrapper = mount(HistoryPage);
    await flushPromises();
    await wrapper.get("[data-action='delete-run']").trigger("click");
    await wrapper.get("[data-action='confirm-delete-run']").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("删不了这条查询历史");
    expect(wrapper.text()).toContain("库被占用");
    expect(wrapper.findAll("[data-action='open-history']")).toHaveLength(1);
    wrapper.unmount();
  });
});
