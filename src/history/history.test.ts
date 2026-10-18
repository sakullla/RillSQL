import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { nextTick } from "vue";
import { afterEach, describe, expect, it, vi } from "vitest";
import { catalogFixture } from "../catalog/fixtures";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import HistoryPage from "./HistoryPage.vue";
import { filterHistory, groupHistoryByDay, openPlan, statusLabel, type HistoryEntry } from "./plan";

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

/** 打开某个 Listbox 并点中目标选项；筛选不再走原生 select。 */
async function chooseOption(wrapper: VueWrapper, ariaLabel: string, label: string): Promise<void> {
  await wrapper.get(`[role="combobox"][aria-label="${ariaLabel}"]`).trigger("click");
  const option = wrapper.findAll('[role="listbox"] [role="option"]').find((node) => node.text().trim() === label);
  if (!option) throw new Error(`Listbox「${ariaLabel}」里没有选项「${label}」`);
  await option.trigger("click");
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

    const buttons = wrapper.findAll("[data-action='open-history']");
    expect(buttons).toHaveLength(2);
    expect(buttons[0]!.text()).toContain("载入编辑器");
    await buttons[0]!.trigger("click");
    const opened = wrapper.emitted("open")?.[0]?.[0] as { execute: boolean; needsTarget: boolean; sql: string };
    expect(opened.execute).toBe(false);
    expect(opened.needsTarget).toBe(true);
    expect(opened.sql).toContain("DELETE");
    expect(invoke.mock.calls.map((call) => call[0])).toEqual(["query_history"]);

    await chooseOption(wrapper, "按结果筛选", "失败");
    expect(wrapper.findAll("[data-action='open-history']")).toHaveLength(1);
    expect(wrapper.text()).not.toContain("旧连接 / app");
    await chooseOption(wrapper, "按结果筛选", "全部结果");
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

  it("右键历史条目：载入不执行、复制 SQL、查看详情、按连接筛选", async () => {
    replaceCatalog(catalogFixture());
    const rows = [
      entry({}),
      entry({ id: "h2", sql: "SELECT 1", connectionId: "shop", connectionName: "Shop", databaseName: "shop_staging", engine: "mysql", startedAt: "2026-10-13T09:00:00", status: "error", summary: "1 条语句失败" }),
    ];
    invoke.mockImplementation(async (command: string) => {
      if (command === "query_history") return rows;
      throw new Error(command);
    });
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    const wrapper = mount(HistoryPage);
    await flushPromises();

    // 第二条挂在已知连接 shop 上，四条菜单项齐全
    const card = wrapper.findAll(".hist-card")[1]!;
    card.element.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 40, clientY: 60 }));
    await nextTick();
    const menu = document.body.querySelector('[role="menu"]');
    expect(menu).toBeTruthy();
    const ids = [...menu!.querySelectorAll('[role="menuitem"]')].map((item) => item.getAttribute("data-id"));
    expect(ids).toEqual(expect.arrayContaining(["load", "copy-sql", "details", "filter-conn"]));

    // 载入编辑器：与「载入编辑器 ↗」一致，只载入不执行
    menu!.querySelector<HTMLButtonElement>('[data-id="load"]')!.click();
    await flushPromises();
    const opened = wrapper.emitted("open")?.[0]?.[0] as { execute: boolean; needsTarget: boolean; sql: string };
    expect(opened.execute).toBe(false);
    expect(opened.needsTarget).toBe(false);
    expect(opened.sql).toBe("SELECT 1");
    expect(invoke.mock.calls.map((call) => call[0])).toEqual(["query_history"]);

    // 复制 SQL
    card.element.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 40, clientY: 60 }));
    await nextTick();
    document.body.querySelector('[role="menu"] [data-id="copy-sql"]')!.click();
    await flushPromises();
    expect(writeText).toHaveBeenCalledWith("SELECT 1");

    // 查看执行详情：inspector 切到该条
    card.element.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 40, clientY: 60 }));
    await nextTick();
    document.body.querySelector('[role="menu"] [data-id="details"]')!.click();
    await nextTick();
    const inspector = wrapper.get(".history-inspector");
    expect(inspector.text()).toContain("Shop");
    expect(inspector.text()).toContain("SELECT 1");

    // 按连接筛选：Listbox 值变为该条连接，列表只剩它的记录
    card.element.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 40, clientY: 60 }));
    await nextTick();
    document.body.querySelector('[role="menu"] [data-id="filter-conn"]')!.click();
    await nextTick();
    expect(wrapper.get('[role="combobox"][aria-label="按连接筛选"]').text().trim()).toBe("Shop");
    expect(wrapper.findAll(".hist-card")).toHaveLength(1);
    wrapper.unmount();
  });

  it("筛选走 Listbox 而非原生下拉，预览面板带拖宽把手，日期分组仍保留", async () => {
    replaceCatalog(catalogFixture());
    const rows = [
      entry({}),
      entry({ id: "h2", sql: "SELECT 1", connectionId: "shop", connectionName: "Shop", databaseName: "shop_staging", engine: "mysql", startedAt: "2026-10-13T09:00:00", status: "error", summary: "1 条语句失败" }),
    ];
    invoke.mockImplementation(async (command: string) => {
      if (command === "query_history") return rows;
      throw new Error(command);
    });
    const wrapper = mount(HistoryPage);
    await flushPromises();

    // 1) 没有原生 select；两个筛选都是带 aria-label 的 Listbox，选项里含 value "all"
    expect(wrapper.findAll("select")).toHaveLength(0);
    expect(wrapper.findAll('[role="combobox"][aria-label="按连接筛选"]')).toHaveLength(1);
    expect(wrapper.findAll('[role="combobox"][aria-label="按结果筛选"]')).toHaveLength(1);
    await wrapper.get('[role="combobox"][aria-label="按连接筛选"]').trigger("click");
    expect(wrapper.findAll('[role="listbox"] [role="option"]').map((node) => node.text().trim())).toEqual([
      "全部连接",
      "原连接已删除",
      "旧连接",
      "Shop",
    ]);
    await wrapper.get('[role="listbox"] [role="option"]').trigger("click");

    // 5) 按日分组的标题仍在
    expect(wrapper.findAll(".hist-day h2").map((node) => node.text())).toEqual(groupHistoryByDay(rows).map((group) => group.label));

    // 用 Listbox 按结果筛选，列表随之收窄
    await chooseOption(wrapper, "按结果筛选", "失败");
    expect(wrapper.findAll(".hist-card")).toHaveLength(1);
    await chooseOption(wrapper, "按结果筛选", "全部结果");
    expect(wrapper.findAll(".hist-card")).toHaveLength(2);

    // 2) 「载入编辑器」仍是主入口，只载入不执行
    await wrapper.get("[data-action='open-history']").trigger("click");
    const opened = wrapper.emitted("open")?.at(-1)?.[0] as { execute: boolean; sql: string };
    expect(opened.execute).toBe(false);
    expect(invoke.mock.calls.map((call) => call[0])).not.toContain("query_start");
    const inspector = wrapper.get(".history-inspector");
    expect(inspector.get("button.btn.primary").text()).toContain("载入编辑器");
    expect(inspector.text()).toContain("只载入 SQL，不会执行");

    // 4) 预览面板带拖宽把手；pointer 拖动不抛错，宽度夹在 280–520px
    const grip = wrapper.get("[data-inspector-grip]");
    expect(grip.attributes("aria-label")).toBeTruthy();
    const widthOf = (): number => {
      const matched = /width:\s*(\d+)px/.exec(wrapper.get(".history-inspector").attributes("style") ?? "");
      return Number(matched?.[1] ?? 0);
    };
    expect(widthOf()).toBeGreaterThanOrEqual(280);
    expect(widthOf()).toBeLessThanOrEqual(520);
    grip.element.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true, cancelable: true, clientX: 400 }));
    window.dispatchEvent(new MouseEvent("pointermove", { clientX: 460 }));
    window.dispatchEvent(new MouseEvent("pointerup", { clientX: 460 }));
    await nextTick();
    expect(widthOf()).toBeGreaterThanOrEqual(280);
    expect(widthOf()).toBeLessThanOrEqual(520);
    grip.element.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true, cancelable: true, clientX: 400 }));
    window.dispatchEvent(new MouseEvent("pointermove", { clientX: 4000 }));
    window.dispatchEvent(new MouseEvent("pointerup", { clientX: 4000 }));
    await nextTick();
    expect(widthOf()).toBe(520);
    grip.element.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true, cancelable: true, clientX: 400 }));
    window.dispatchEvent(new MouseEvent("pointermove", { clientX: 0 }));
    window.dispatchEvent(new MouseEvent("pointerup", { clientX: 0 }));
    await nextTick();
    expect(widthOf()).toBe(280);

    // 3) 右键条目仍然弹出 [role="menu"]
    wrapper.findAll(".hist-card")[0]!.element.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 30, clientY: 40 }));
    await nextTick();
    expect(document.body.querySelector('[role="menu"]')).toBeTruthy();
    wrapper.unmount();
  });
});
