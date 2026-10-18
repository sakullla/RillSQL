import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import EmptyState from "./EmptyState.vue";

describe("EmptyState", () => {
  it("renders the title and detail text", () => {
    const wrapper = mount(EmptyState, {
      props: { title: "还没有连接", detail: "先创建一个数据库连接再开始。" },
    });
    expect(wrapper.get(".empty-state-title").text()).toBe("还没有连接");
    expect(wrapper.get(".empty-state-detail").text()).toBe("先创建一个数据库连接再开始。");
    expect(wrapper.attributes("data-empty-state")).toBe("");
  });

  it("renders an action button that emits action when actionLabel is set", async () => {
    const wrapper = mount(EmptyState, {
      props: { title: "还没有连接", detail: "先创建一个数据库连接再开始。", actionLabel: "新建连接", actionId: "create" },
    });
    const button = wrapper.get("button[data-action]");
    expect(button.attributes("data-action")).toBe("create");
    expect(button.text()).toBe("新建连接");
    await button.trigger("click");
    expect(wrapper.emitted("action")).toHaveLength(1);
  });

  it("does not render an action button without actionLabel", () => {
    const wrapper = mount(EmptyState, {
      props: { title: "没有结果", detail: "换一个筛选条件试试。" },
    });
    expect(wrapper.find("button").exists()).toBe(false);
  });
});
