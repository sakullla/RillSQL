import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import Toolbar from "./Toolbar.vue";

function open(hasQueryTab = true) {
  return mount(Toolbar, { props: { hasQueryTab, theme: "light" }, attachTo: document.body });
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
});
