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

  it("defaults to a 400ms delay", async () => {
    const wrapper = mount(Tooltip, {
      props: { text: "导出" },
      slots: { default: '<button class="icon-btn" aria-label="导出">e</button>' },
      attachTo: document.body,
    });
    await wrapper.find(".tip-host").trigger("mouseenter");
    vi.advanceTimersByTime(399);
    await nextTick();
    expect(wrapper.find('[role="tooltip"]').exists()).toBe(false);
    vi.advanceTimersByTime(2);
    await nextTick();
    expect(wrapper.find('[role="tooltip"]').exists()).toBe(true);
    wrapper.unmount();
  });

  it("cancels the pending timer when the pointer leaves before the delay", async () => {
    const wrapper = mount(Tooltip, {
      props: { text: "取消", delay: 400 },
      slots: { default: '<button class="icon-btn" aria-label="取消">x</button>' },
      attachTo: document.body,
    });
    await wrapper.find(".tip-host").trigger("mouseenter");
    vi.advanceTimersByTime(200);
    await wrapper.find(".tip-host").trigger("mouseleave");
    // 若 hide() 里没有 clearTimeout，这里会迟到出现
    vi.advanceTimersByTime(500);
    await nextTick();
    expect(wrapper.find('[role="tooltip"]').exists()).toBe(false);
    wrapper.unmount();
  });
});
