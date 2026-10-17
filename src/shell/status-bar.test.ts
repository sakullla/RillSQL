import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import StatusBar from "./StatusBar.vue";
import type { ShellTab } from "./session";

function queryTab(): ShellTab {
  return {
    id: "t1",
    kind: "query",
    title: "查询 1",
    draft: "",
    connectionId: "c1",
    connectionName: "书店 · 开发",
    database: "bookstore",
    engine: "postgres",
    status: "connected",
    targetText: "书店 · 开发 / bookstore",
    connectionStatus: "connected",
  } as unknown as ShellTab;
}

describe("StatusBar", () => {
  it("renders the target as a clickable segment that navigates to connections", async () => {
    const wrapper = mount(StatusBar, { props: { tab: queryTab(), onWorkspace: true } });
    const segment = wrapper.get('[data-segment="connection"]');
    expect(segment.attributes("role")).toBe("button");
    await segment.trigger("click");
    expect(wrapper.emitted("navigate")?.[0]).toEqual(["connections"]);
  });

  it("keeps the product title as a non-clickable segment when nothing is open", () => {
    const wrapper = mount(StatusBar, { props: { tab: null, onWorkspace: false } });
    expect(wrapper.find('[data-segment="connection"]').exists()).toBe(false);
    expect(wrapper.text()).toContain("RillSQL");
  });

  it("exposes a history segment on the right", async () => {
    const wrapper = mount(StatusBar, { props: { tab: null, onWorkspace: true } });
    await wrapper.get('[data-segment="history"]').trigger("click");
    expect(wrapper.emitted("navigate")?.[0]).toEqual(["history"]);
  });
});
