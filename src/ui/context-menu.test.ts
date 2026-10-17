import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import { nextTick } from "vue";
import ContextMenu from "./ContextMenu.vue";
import type { MenuItem } from "./types";

const ITEMS: MenuItem[] = [
  { kind: "head", text: "orders · 第 2 行" },
  { kind: "item", id: "open-data", label: "打开表数据", shortcut: "Enter" },
  { kind: "item", id: "copy", label: "复制限定名", shortcut: "Ctrl+C" },
  { kind: "item", id: "stale", label: "标记删除", disabled: true },
  { kind: "sep" },
  { kind: "item", id: "drop", label: "删除表…", danger: true },
];

function open(x = 100, y = 100) {
  return mount(ContextMenu, {
    props: { open: true, x, y, items: ITEMS, label: "对象操作" },
    attachTo: document.body,
    // VTU 不追踪 Teleport 落点，打桩后内容留在组件树内，wrapper.get 才能查到
    global: { stubs: { teleport: true } },
  });
}

describe("ContextMenu", () => {
  it("renders head, items, separators and danger styling", async () => {
    const wrapper = open();
    await nextTick();
    expect(wrapper.get('[role="menu"]').attributes("aria-label")).toBe("对象操作");
    expect(wrapper.get(".menu-head").text()).toBe("orders · 第 2 行");
    expect(wrapper.findAll('[role="menuitem"]')).toHaveLength(4);
    expect(wrapper.find(".menu-sep").exists()).toBe(true);
    expect(wrapper.get('[data-id="drop"]').classes()).toContain("danger");
    wrapper.unmount();
  });

  it("clamps the position so the menu stays on screen", async () => {
    const wrapper = open(window.innerWidth + 400, window.innerHeight + 400);
    await nextTick();
    const style = wrapper.get('[role="menu"]').attributes("style") ?? "";
    const left = Number(/left: (-?\d+)px/.exec(style)?.[1]);
    const top = Number(/top: (-?\d+)px/.exec(style)?.[1]);
    expect(left).toBe(window.innerWidth - 208 - 8);
    expect(top).toBeLessThanOrEqual(window.innerHeight - 8);
    expect(top).toBeGreaterThanOrEqual(8);
    wrapper.unmount();
  });

  it("focuses the first item on open and walks with arrows, skipping disabled", async () => {
    const wrapper = open();
    await nextTick();
    await nextTick();
    const menu = wrapper.get('[role="menu"]');
    expect(document.activeElement?.textContent).toContain("打开表数据");
    await menu.trigger("keydown", { key: "ArrowDown" });
    expect(document.activeElement?.textContent).toContain("复制限定名");
    await menu.trigger("keydown", { key: "ArrowDown" });
    // 第三项 disabled，应跳到「删除表…」
    expect(document.activeElement?.textContent).toContain("删除表…");
    wrapper.unmount();
  });

  it("emits select with the item id then closes", async () => {
    const wrapper = open();
    await nextTick();
    await nextTick();
    await wrapper.get('[data-id="copy"]').trigger("click");
    expect(wrapper.emitted("select")?.[0]).toEqual(["copy"]);
    expect(wrapper.emitted("close")).toHaveLength(1);
    wrapper.unmount();
  });

  it("does not emit select for a disabled item", async () => {
    const wrapper = open();
    await nextTick();
    await wrapper.get('[data-id="stale"]').trigger("click");
    expect(wrapper.emitted("select")).toBeUndefined();
    wrapper.unmount();
  });

  it("closes on Escape", async () => {
    const wrapper = open();
    await nextTick();
    await wrapper.get('[role="menu"]').trigger("keydown", { key: "Escape" });
    expect(wrapper.emitted("close")).toHaveLength(1);
    wrapper.unmount();
  });

  it("closes on an outside pointer, but not on a pointer inside the menu", async () => {
    const wrapper = open();
    await nextTick();
    // 菜单内部的 pointerdown 不应关闭
    wrapper.get('[role="menu"]').element.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    await nextTick();
    expect(wrapper.emitted("close")).toBeUndefined();
    // 菜单外部的 pointerdown 必须关闭
    document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    await nextTick();
    expect(wrapper.emitted("close")).toHaveLength(1);
    wrapper.unmount();
  });

  it("Home and End jump to the first and last enabled items", async () => {
    const wrapper = open();
    await nextTick();
    await nextTick();
    await wrapper.get('[role="menu"]').trigger("keydown", { key: "End" });
    expect(document.activeElement?.textContent).toContain("删除表…");
    await wrapper.get('[role="menu"]').trigger("keydown", { key: "Home" });
    expect(document.activeElement?.textContent).toContain("打开表数据");
    wrapper.unmount();
  });

  it("restores focus to the element that opened it", async () => {
    const source = document.createElement("button");
    source.textContent = "右键来源";
    document.body.append(source);
    source.focus();
    const wrapper = mount(ContextMenu, {
      props: { open: false, x: 40, y: 40, items: ITEMS, label: "对象操作" },
      global: { stubs: { teleport: true } },
      attachTo: document.body,
    });
    await wrapper.setProps({ open: true });
    await nextTick();
    await nextTick();
    expect(document.activeElement?.textContent).toContain("打开表数据");
    await wrapper.setProps({ open: false });
    await nextTick();
    expect(document.activeElement).toBe(source);
    source.remove();
    wrapper.unmount();
  });

  it("closes on Tab without swallowing the keystroke", async () => {
    const wrapper = open();
    await nextTick();
    const event = new KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
    wrapper.get('[role="menu"]').element.dispatchEvent(event);
    await nextTick();
    expect(wrapper.emitted("close")).toHaveLength(1);
    expect(event.defaultPrevented).toBe(false);
    wrapper.unmount();
  });
});
