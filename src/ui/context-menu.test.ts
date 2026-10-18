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

  it("does not close when the pointer is on the excluded trigger", async () => {
    const trigger = document.createElement("button");
    document.body.append(trigger);
    const wrapper = mount(ContextMenu, {
      props: { open: true, x: 40, y: 40, items: ITEMS, label: "对象操作", trigger },
      global: { stubs: { teleport: true } },
      attachTo: document.body,
    });
    await nextTick();
    trigger.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    await nextTick();
    expect(wrapper.emitted("close")).toBeUndefined();
    document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    await nextTick();
    expect(wrapper.emitted("close")).toHaveLength(1);
    trigger.remove();
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

  it("accounts for the standard-density row height when clamping", async () => {
    // 模拟 standard 密度：--control-h-sm 变成 26px
    document.documentElement.style.setProperty("--control-h-sm", "26px");
    try {
      const items: MenuItem[] = [
        { kind: "item", id: "a", label: "A" },
        { kind: "item", id: "b", label: "B" },
        { kind: "item", id: "c", label: "C" },
      ];
      const y = 200;
      const wrapper = mount(ContextMenu, {
        props: { open: true, x: 40, y, items, label: "对象操作" },
        global: { stubs: { teleport: true } },
        attachTo: document.body,
      });
      await nextTick();
      const style = wrapper.get('[role="menu"]').attributes("style") ?? "";
      const top = Number(/top: (-?\d+)px/.exec(style)?.[1]);
      // 26px 行高 × 3 项 + 12 = 90；y=200 不会触底，所以 top 应等于 y 本身
      expect(top).toBe(y);
      // 把 y 压到触底，验证估算确实变大了（26 行高比 24 把菜单“推”得更高）
      await wrapper.setProps({ y: window.innerHeight - 40 });
      await nextTick();
      const clampedTop = Number(/top: (-?\d+)px/.exec(wrapper.get('[role="menu"]').attributes("style") ?? "")?.[1]);
      // 估算高度 3×26+12 = 90 → 可用 top 上限 = innerHeight - 90 - 8
      expect(clampedTop).toBeLessThanOrEqual(window.innerHeight - 90 - 8);
      expect(clampedTop).toBeGreaterThanOrEqual(8);
      wrapper.unmount();
    } finally {
      document.documentElement.style.removeProperty("--control-h-sm");
    }
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

  it("formats Mod chords for the current platform in the shortcut column", async () => {
    // jsdom 在 Windows 上，detectMac() 为 false，Mod 应显示为 Ctrl
    const wrapper = mount(ContextMenu, {
      props: {
        open: true,
        x: 40,
        y: 40,
        items: [{ kind: "item", id: "new", label: "新建查询", shortcut: "Mod+T" }],
      },
      global: { stubs: { teleport: true } },
      attachTo: document.body,
    });
    await nextTick();
    expect(wrapper.get(".menu-key").text()).toBe("Ctrl+T");
    wrapper.unmount();
  });

  it("renders a check mark on checked items", async () => {
    const wrapper = mount(ContextMenu, {
      props: {
        open: true,
        x: 40,
        y: 40,
        label: "外观",
        items: [
          { kind: "item", id: "light", label: "浅色", checked: true },
          { kind: "item", id: "dark", label: "深色" },
        ],
      },
      global: { stubs: { teleport: true } },
      attachTo: document.body,
    });
    await nextTick();
    expect(wrapper.find('[data-id="light"] .check').exists()).toBe(true);
    expect(wrapper.find('[data-id="dark"] .check').exists()).toBe(false);
    expect(wrapper.find('[data-id="light"]').attributes("aria-checked")).toBe("true");
    expect(wrapper.find('[data-id="dark"]').attributes("aria-checked")).toBeUndefined();
    wrapper.unmount();
  });
});
