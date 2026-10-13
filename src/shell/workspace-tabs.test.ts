import { EditorView } from "@codemirror/view";
import { mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import { createSession } from "./session";

vi.mock("../catalog", () => ({
  listPaletteTables: () => [{ id: "customers", label: "customers", path: "shop / public" }],
}));

import AppShell from "./AppShell.vue";

afterEach(() => {
  location.hash = "";
});

function editorText(wrapper: VueWrapper): string {
  const view = EditorView.findFromDOM(wrapper.get(".cm-content").element);
  expect(view).toBeTruthy();
  return view!.state.doc.toString();
}

async function replaceDraft(wrapper: VueWrapper, text: string) {
  const view = EditorView.findFromDOM(wrapper.get(".cm-content").element);
  expect(view).toBeTruthy();
  view!.dispatch({ changes: { from: 0, to: view!.state.doc.length, insert: text } });
  await nextTick();
}

function paletteItem(wrapper: VueWrapper, label: string) {
  const item = wrapper.findAll(".pal-item").find((node) => node.text().includes(label));
  expect(item, label).toBeTruthy();
  return item!;
}

describe("workspace tab identity", () => {
  it("keeps query drafts on the tab that owns them", async () => {
    const wrapper = mount(AppShell);
    await wrapper.get(".wl-item").trigger("click");
    expect(wrapper.get("[aria-label='查询']").attributes("data-tab-id")).toBe("query-1");
    await replaceDraft(wrapper, "SELECT alpha");
    await wrapper.get(".tab-new").trigger("click");
    expect(wrapper.get("[aria-label='查询']").attributes("data-tab-id")).toBe("query-2");
    await replaceDraft(wrapper, "SELECT beta");

    await wrapper.findAll(".tab")[0]!.trigger("click");
    await nextTick();
    expect(wrapper.get("[aria-label='查询']").attributes("data-tab-id")).toBe("query-1");
    expect(editorText(wrapper)).toBe("SELECT alpha");

    await wrapper.findAll(".tab")[1]!.trigger("click");
    await nextTick();
    expect(editorText(wrapper)).toBe("SELECT beta");

    await wrapper.get(".tab.active .tab-x").trigger("click");
    await nextTick();
    expect(wrapper.findAll(".tab")).toHaveLength(1);
    expect(wrapper.get(".tab-title").text()).toBe("查询 1");
    expect(wrapper.get("[aria-label='查询']").attributes("data-tab-id")).toBe("query-1");
    expect(editorText(wrapper)).toBe("SELECT alpha");
    wrapper.unmount();
  });

  it("opens table and structure placeholders from welcome and the command palette", async () => {
    const welcome = mount(AppShell);
    const structure = welcome.findAll(".wl-item").find((item) => item.text().includes("打开结构占位"));
    expect(structure).toBeTruthy();
    await structure!.trigger("click");
    expect(welcome.get("[aria-label='表结构']").attributes("data-tab-id")).toBe("structure:placeholder");
    expect(welcome.get("[aria-label='表结构']").attributes("data-draft")).toBe("");
    expect(welcome.get(".tab-title").text()).toBe("结构占位");
    welcome.unmount();

    const wrapper = mount(AppShell);
    await wrapper.get(".cmd-trigger").trigger("click");
    await paletteItem(wrapper, "打开结构占位").trigger("click");
    expect(wrapper.get("[aria-label='表结构']").attributes("data-tab-id")).toBe("structure:placeholder");

    await wrapper.get(".cmd-trigger").trigger("click");
    await paletteItem(wrapper, "customers").trigger("click");
    expect(wrapper.findAll(".tab")).toHaveLength(2);
    const grid = wrapper.get("[aria-label='数据表格']");
    expect(grid.attributes("data-tab-id")).toBe("table:customers");
    expect(grid.attributes("data-draft")).toBe("");
    expect(wrapper.get(".tab.active .tab-title").text()).toBe("customers");
    expect(wrapper.get(".tab.active .tab-conn").text()).toBe("shop / public");
    expect(wrapper.get(".statusbar").text()).toContain("未绑定连接");

    await wrapper.get(".cmd-trigger").trigger("click");
    await paletteItem(wrapper, "customers").trigger("click");
    expect(wrapper.findAll(".tab")).toHaveLength(2);

    await wrapper.findAll(".tab")[0]!.trigger("click");
    await nextTick();
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
    session.openStructure({ id: "placeholder", label: "结构占位" });
    expect(session.activeTab.value).toMatchObject({ id: "structure:placeholder", kind: "structure" });
  });
});
