import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { mount, type VueWrapper } from "@vue/test-utils";
import { describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import Toolbar from "./Toolbar.vue";

const here = dirname(fileURLToPath(import.meta.url));

// detectMac 结果由用例控制：默认钉 Windows 形态，macOS 形态用 mock 断言
const platform = vi.hoisted(() => ({ mac: false }));
vi.mock("./shortcuts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./shortcuts")>();
  return { ...actual, detectMac: () => platform.mac };
});

function open(hasQueryTab = true) {
  return mount(Toolbar, { props: { hasQueryTab, theme: "light" }, attachTo: document.body });
}

async function tipText(wrapper: VueWrapper, action: string): Promise<string> {
  const host = wrapper
    .findAll(".tip-host")
    .find((node) => node.find(`[data-action="${action}"]`).exists());
  expect(host, `no tooltip host for ${action}`).toBeTruthy();
  await host!.trigger("mouseenter");
  vi.advanceTimersByTime(401);
  await nextTick();
  return host!.get('[role="tooltip"]').text();
}

describe("Toolbar", () => {
  it("renders icon-first actions grouped by separators", () => {
    const wrapper = open();
    expect(wrapper.findAll(".tb-group")).toHaveLength(3);
    expect(wrapper.findAll(".tb-sep")).toHaveLength(2);
    wrapper.unmount();
  });

  it("centers default-slot content between the format and theme groups", () => {
    const wrapper = mount(Toolbar, {
      props: { hasQueryTab: true, theme: "light" },
      slots: { default: '<button class="cmd-trigger" type="button">搜索</button>' },
      attachTo: document.body,
    });
    expect(wrapper.find(".tb-center .cmd-trigger").text()).toBe("搜索");
    expect(wrapper.findAll(".tb-group")).toHaveLength(3);
    wrapper.unmount();
  });

  it("emits the matching action id for each enabled button", async () => {
    const wrapper = open();
    const fire = async (action: string) => {
      await wrapper.get(`[data-action="${action}"]`).trigger("click");
      expect(wrapper.emitted("action")?.at(-1)).toEqual([action]);
    };
    await fire("new-query");
    await fire("open-connections");
    await fire("history");
    await fire("palette");
    await fire("tasks");
    await fire("run");
    wrapper.unmount();
  });

  it("disables query actions when there is no query tab", () => {
    const wrapper = open(false);
    for (const action of ["format", "plan", "run"]) {
      expect(wrapper.get(`[data-action="${action}"]`).attributes("disabled")).toBe("");
    }
    wrapper.unmount();
  });

  it("labels every icon-only control for assistive tech", () => {
    const wrapper = open();
    for (const button of wrapper.findAll("button")) {
      expect(button.attributes("aria-label"), button.attributes("data-action") ?? button.text()).toBeTruthy();
    }
    wrapper.unmount();
  });

  it("derives shortcut hints from the shortcut table instead of hardcoding", async () => {
    platform.mac = false;
    vi.useFakeTimers();
    try {
      const wrapper = open();
      expect(await tipText(wrapper, "new-query")).toBe("新建查询 (Ctrl+T)");
      expect(await tipText(wrapper, "format")).toBe("格式化 (Ctrl+Shift+F)");
      expect(await tipText(wrapper, "plan")).toBe("执行计划 (Ctrl+Shift+E)");
      expect(await tipText(wrapper, "palette")).toBe("命令面板 (Ctrl+K)");
      expect(wrapper.get('[data-action="run"]').text()).toContain("Ctrl+Enter");
      wrapper.unmount();
      const source = readFileSync(resolve(here, "Toolbar.vue"), "utf8");
      expect(source).not.toContain('text="新建查询 (Ctrl+T)"');
      expect(source).toContain("chordByLabel");
    } finally {
      vi.useRealTimers();
    }
  });

  it("shows ⌘ chords in tooltips and the run kbd chip when detectMac is true", async () => {
    platform.mac = true;
    vi.useFakeTimers();
    try {
      const wrapper = open();
      expect(await tipText(wrapper, "new-query")).toBe("新建查询 (⌘+T)");
      expect(await tipText(wrapper, "format")).toBe("格式化 (⌘+Shift+F)");
      expect(wrapper.get('[data-action="run"]').text()).toContain("⌘+Enter");
      wrapper.unmount();
    } finally {
      vi.useRealTimers();
      platform.mac = false;
    }
  });

  it("keeps data-action ids shared with the query pane (scoped queries are mandatory)", () => {
    const pane = readFileSync(resolve(here, "../query/QueryTab.vue"), "utf8");
    const toolbar = readFileSync(resolve(here, "Toolbar.vue"), "utf8");
    // 这是已知的重名契约：两个面板都有 run/format/plan，页面级查询必须限定作用域
    for (const id of ["run", "format", "plan"]) {
      expect(toolbar, `toolbar 缺 ${id}`).toContain(`data-action="${id}"`);
      expect(pane, `QueryTab 缺 ${id}`).toContain(`data-action="${id}"`);
    }
  });
});
