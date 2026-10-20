import { EditorView } from "@codemirror/view";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import { resetCatalog, replaceCatalog } from "../catalog/store";
import type { CatalogConnection } from "../catalog/types";
import { createSession } from "./session";

vi.mock("../catalog", () => ({
  listPaletteTables: () => [{ id: "customers", label: "customers", path: "shop / public" }],
  startCatalog: () => Promise.resolve(),
}));

import AppShell from "./AppShell.vue";

afterEach(() => {
  location.hash = "";
  localStorage.clear();
  resetCatalog();
});

function contextMenu(target: Element, x = 40, y = 28): MouseEvent {
  const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: x, clientY: y });
  target.dispatchEvent(event);
  return event;
}

function tabMenu(): HTMLElement {
  const menu = document.body.querySelector("[data-menu=tab]");
  expect(menu).toBeTruthy();
  return menu as HTMLElement;
}

function postgresCatalog(): CatalogConnection {
  return {
    id: "pg",
    name: "127.0.0.1",
    engine: "postgres",
    env: "dev",
    envLabel: "开发",
    envTone: "dev",
    status: "connected",
    defaultDatabase: "rillsql",
    foreignKeys: null,
    error: null,
    databases: [
      { name: "rillsql", schemas: [], error: null },
      { name: "postgres", schemas: [], error: null },
    ],
  };
}

function editorText(wrapper: VueWrapper): string {
  const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement);
  expect(view).toBeTruthy();
  return view!.state.doc.toString();
}

async function replaceDraft(wrapper: VueWrapper, text: string) {
  const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement);
  expect(view).toBeTruthy();
  view!.dispatch({ changes: { from: 0, to: view!.state.doc.length, insert: text } });
  await nextTick();
    await vi.dynamicImportSettled();
    await flushPromises();
}

function paletteItem(wrapper: VueWrapper, label: string) {
  const item = wrapper.findAll(".pal-item").find((node) => node.text().includes(label));
  expect(item, label).toBeTruthy();
  return item!;
}

async function openCommand(wrapper: VueWrapper, label: string) {
  await wrapper.get(".cmd-trigger").trigger("click");
  await vi.dynamicImportSettled();
  await flushPromises();
  await paletteItem(wrapper, label).trigger("click");
  await vi.dynamicImportSettled();
  await flushPromises();
}

describe("workspace tab identity", () => {
  it("keeps query drafts on the tab that owns them", async () => {
    const wrapper = mount(AppShell);
    await openCommand(wrapper, "新建查询");
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(wrapper.get("[aria-label='查询']").attributes("data-tab-id")).toBe("query-1");
    await replaceDraft(wrapper, "SELECT alpha");
    await wrapper.get(".tab-new").trigger("click");
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(wrapper.get("[aria-label='查询']").attributes("data-tab-id")).toBe("query-2");
    await replaceDraft(wrapper, "SELECT beta");

    await wrapper.findAll(".tab")[0]!.trigger("click");
    await vi.dynamicImportSettled();
    await flushPromises();
    await nextTick();
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(wrapper.get("[aria-label='查询']").attributes("data-tab-id")).toBe("query-1");
    expect(editorText(wrapper)).toBe("SELECT alpha");

    await wrapper.findAll(".tab")[1]!.trigger("click");
    await vi.dynamicImportSettled();
    await flushPromises();
    await nextTick();
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(editorText(wrapper)).toBe("SELECT beta");

    await wrapper.get(".tab.active .tab-x").trigger("click");
    await vi.dynamicImportSettled();
    await flushPromises();
    // query-2 的草稿非空，现在会弹挽留框；选择离开丢弃草稿并关闭标签
    await wrapper.get("[data-action='close-leave']").trigger("click");
    await nextTick();
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(wrapper.findAll(".tab")).toHaveLength(1);
    expect(wrapper.get(".tab-title").text()).toBe("查询 1");
    expect(wrapper.get("[aria-label='查询']").attributes("data-tab-id")).toBe("query-1");
    expect(editorText(wrapper)).toBe("SELECT alpha");
    wrapper.unmount();
  });

  it("opens table and structure tabs from the command palette", async () => {
    const welcome = mount(AppShell);
    await openCommand(welcome, "新建表结构");
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(welcome.get("[aria-label='表结构']").attributes("data-tab-id")).toBe("structure:placeholder");
    expect(welcome.get("[aria-label='表结构']").attributes("data-draft")).toBe("");
    expect(welcome.get(".tab-title").text()).toBe("新建表结构");
    welcome.unmount();

    const wrapper = mount(AppShell);
    await wrapper.get(".cmd-trigger").trigger("click");
    await vi.dynamicImportSettled();
    await flushPromises();
    await paletteItem(wrapper, "新建表结构").trigger("click");
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(wrapper.get("[aria-label='表结构']").attributes("data-tab-id")).toBe("structure:placeholder");

    await wrapper.get(".cmd-trigger").trigger("click");
    await vi.dynamicImportSettled();
    await flushPromises();
    await paletteItem(wrapper, "customers").trigger("click");
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(wrapper.findAll(".tab")).toHaveLength(2);
    const grid = wrapper.get("[aria-label='数据表格']");
    expect(grid.attributes("data-tab-id")).toBe("table:customers");
    expect(grid.attributes("data-draft")).toBe("");
    expect(wrapper.get(".tab.active .tab-title").text()).toBe("customers");
    // 标签与状态栏用文字说明数据来自哪里（R4）
    expect(wrapper.get(".tab.active .tab-target").text()).toBe("shop / public");
    expect(wrapper.get(".statusbar").text()).toContain("shop / public");

    await wrapper.get(".cmd-trigger").trigger("click");
    await vi.dynamicImportSettled();
    await flushPromises();
    await paletteItem(wrapper, "customers").trigger("click");
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(wrapper.findAll(".tab")).toHaveLength(2);

    await wrapper.findAll(".tab")[0]!.trigger("click");
    await vi.dynamicImportSettled();
    await flushPromises();
    await nextTick();
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(wrapper.get("[aria-label='表结构']").attributes("data-tab-id")).toBe("structure:placeholder");
    expect(wrapper.find("[aria-label='数据表格']").exists()).toBe(false);
    wrapper.unmount();
  });

  it("binds a table tab to the id returned for that table", () => {
    const session = createSession("#/");
    session.openTable({ id: "customers", label: "customers", path: "shop / public" });
    session.openQuery();
    session.openTable({ id: "customers", label: "customers", path: "shop / public" });
    expect(session.tabs.value.map((tab) => tab.id)).toEqual(["table:customers", "query-1"]);
    expect(session.activeId.value).toBe("table:customers");
    expect(session.tabs.value[0]).toMatchObject({ kind: "table", title: "customers", path: "shop / public", draft: "" });
    session.openStructure({ id: "placeholder", label: "新建表结构" });
    expect(session.activeTab.value).toMatchObject({ id: "structure:placeholder", kind: "structure" });
  });

  it("reuses a table tab and can open it on the drop section", () => {
    const session = createSession("#/");
    session.openTable({ id: "atlas:rillsql:public:customers", label: "customers", path: "rillsql / public", pane: "data" });
    session.openTable({ id: "atlas:rillsql:public:customers", label: "customers", path: "rillsql / public", pane: "structure", section: "drop" });
    expect(session.tabs.value).toHaveLength(1);
    expect(session.tabs.value[0]).toMatchObject({ tablePane: "structure", structureSection: "drop" });
    session.setTableView("table:atlas:rillsql:public:customers", "data");
    expect(session.tabs.value[0]?.tablePane).toBe("data");
    expect(session.tabs.value[0]?.structureSection).toBe("drop");
  });

  it("binds a query from the tab menu and closes the other tabs", async () => {
    replaceCatalog([postgresCatalog()]);
    const wrapper = mount(AppShell);
    expect(wrapper.find(".desk-db").exists()).toBe(false);
    expect(wrapper.get("[aria-label='数据库对象']").text()).toContain("127.0.0.1");
    expect(wrapper.get("[aria-label='数据库对象']").text()).toContain("开发");
    await openCommand(wrapper, "新建查询");
    await vi.dynamicImportSettled();
    await flushPromises();
    await wrapper.get(".tab-new").trigger("click");
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(wrapper.findAll(".tab")).toHaveLength(2);

    const plus = contextMenu(wrapper.get(".tab-new").element);
    await nextTick();
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(plus.defaultPrevented).toBe(true);
    expect(document.body.querySelector("[data-menu=tab]")).toBeNull();

    const first = wrapper.findAll(".tab")[0]!;
    expect(first.get(".tab-target").text()).toBe("未选择连接");
    expect(wrapper.get(".statusbar").text()).toContain("还未选择执行目标");
    const opened = contextMenu(first.element);
    await nextTick();
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(opened.defaultPrevented).toBe(true);
    const menu = tabMenu();
    expect(menu.textContent).toContain("选择执行目标");
    expect(menu.textContent).toContain("127.0.0.1 / rillsql");
    expect(menu.textContent).toContain("127.0.0.1 / postgres");
    expect(menu.textContent).toContain("关闭其他");
    expect(menu.querySelector<HTMLButtonElement>("[data-action=close-right]")?.disabled).toBe(false);

    menu.querySelector<HTMLButtonElement>("[data-action=bind-target]")?.click();
    await nextTick();
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(document.body.querySelector("[data-menu=tab]")).toBeNull();
    expect(wrapper.findAll(".tab")[0]!.get(".tab-target").text()).toBe("127.0.0.1 / rillsql");
    expect(wrapper.get(".statusbar").text()).toContain("127.0.0.1 / rillsql");
    expect(wrapper.get(".statusbar").text()).toContain("已连接");

    contextMenu(wrapper.findAll(".tab")[0]!.element);
    await nextTick();
    await vi.dynamicImportSettled();
    await flushPromises();
    const rebound = tabMenu();
    const picked = rebound.querySelector("[data-key]") as HTMLElement | null;
    expect(picked?.querySelector(".check")).toBeTruthy();
    rebound.querySelector<HTMLButtonElement>("[data-action=close-others]")?.click();
    await nextTick();
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(wrapper.findAll(".tab")).toHaveLength(1);
    expect(wrapper.get(".tab-title").text()).toBe("查询 1");
    expect(wrapper.get(".tab-target").text()).toBe("127.0.0.1 / rillsql");

    contextMenu(wrapper.get(".tab").element);
    await nextTick();
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(tabMenu().querySelector<HTMLButtonElement>("[data-action=close-others]")?.disabled).toBe(true);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    await nextTick();
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(document.body.querySelector("[data-menu=tab]")).toBeNull();
    wrapper.unmount();
  });

  it("switches a table tab between data and structure from the tab menu", async () => {
    const wrapper = mount(AppShell);
    await wrapper.get(".cmd-trigger").trigger("click");
    await vi.dynamicImportSettled();
    await flushPromises();
    await paletteItem(wrapper, "customers").trigger("click");
    await vi.dynamicImportSettled();
    await flushPromises();
    const opened = contextMenu(wrapper.get(".tab").element);
    await nextTick();
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(opened.defaultPrevented).toBe(true);
    const menu = tabMenu();
    expect(menu.textContent).toContain("数据");
    expect(menu.textContent).toContain("结构");
    expect(menu.textContent).not.toContain("选择执行目标");
    menu.querySelector<HTMLButtonElement>("[data-action=show-structure]")?.click();
    await nextTick();
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(document.body.querySelector("[data-menu=tab]")).toBeNull();
    expect(wrapper.get("[aria-label='表结构']").attributes("data-tab-id")).toBe("table:customers");
    expect(wrapper.find("[aria-label='数据表格']").exists()).toBe(false);

    contextMenu(wrapper.get(".tab").element);
    await nextTick();
    await vi.dynamicImportSettled();
    await flushPromises();
    tabMenu().querySelector<HTMLButtonElement>("[data-action=show-data]")?.click();
    await nextTick();
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(wrapper.get("[aria-label='数据表格']").attributes("data-tab-id")).toBe("table:customers");
    wrapper.unmount();
  });
});
