import { mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { nextTick } from "vue";
import Tooltip from "./Tooltip.vue";

describe("Tooltip", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("stays hidden until the delay elapses after hover", async () => {
    const wrapper = mount(Tooltip, {
      props: { text: "格式化 (Ctrl+Shift+F)", delay: 400 },
      slots: { default: '<button class="icon-btn" aria-label="格式化">f</button>' },
      attachTo: document.body,
    });
    expect(wrapper.find('[role="tooltip"]').exists()).toBe(false);
    await wrapper.find(".tip-host").trigger("mouseenter");
    expect(wrapper.find('[role="tooltip"]').exists()).toBe(false);
    vi.advanceTimersByTime(401);
    await nextTick();
    expect(wrapper.find('[role="tooltip"]').exists()).toBe(true);
    expect(wrapper.get('[role="tooltip"]').text()).toBe("格式化 (Ctrl+Shift+F)");
    wrapper.unmount();
  });

  it("hides again on mouse leave before or after showing", async () => {
    const wrapper = mount(Tooltip, {
      props: { text: "执行", delay: 100 },
      slots: { default: '<button class="icon-btn" aria-label="执行">run</button>' },
      attachTo: document.body,
    });
    await wrapper.find(".tip-host").trigger("mouseenter");
    vi.advanceTimersByTime(101);
    await nextTick();
    expect(wrapper.find('[role="tooltip"]').exists()).toBe(true);
    await wrapper.find(".tip-host").trigger("mouseleave");
    expect(wrapper.find('[role="tooltip"]').exists()).toBe(false);
    vi.advanceTimersByTime(200);
    await nextTick();
    expect(wrapper.find('[role="tooltip"]').exists()).toBe(false);
    wrapper.unmount();
  });

  it("renders nothing when text is empty", async () => {
    const wrapper = mount(Tooltip, {
      props: { text: "" },
      slots: { default: '<button class="icon-btn" aria-label="刷新">r</button>' },
      attachTo: document.body,
    });
    await wrapper.find(".tip-host").trigger("mouseenter");
    vi.advanceTimersByTime(1000);
    await nextTick();
    expect(wrapper.find('[role="tooltip"]').exists()).toBe(false);
    wrapper.unmount();
  });
});
