import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import { nextTick } from "vue";
import MenuBar from "./MenuBar.vue";
import type { MenuDefinition } from "./menu-model";

const MENUS: MenuDefinition[] = [
  {
    id: "file",
    label: "文件",
    items: [
      { kind: "item", id: "new-query", label: "新建查询", shortcut: "Ctrl+T" },
      { kind: "item", id: "open-sqlite", label: "打开 SQLite…" },
    ],
  },
  {
    id: "edit",
    label: "编辑",
    items: [
      { kind: "item", id: "palette", label: "命令面板", shortcut: "Ctrl+K" },
      { kind: "item", id: "cut", label: "剪切", disabled: true },
    ],
  },
];

function open() {
  return mount(MenuBar, {
    props: { menus: MENUS },
    attachTo: document.body,
    // VTU 不追踪 Teleport 落点，打桩后内容留在组件树内，wrapper.get 才能查到
    global: { stubs: { teleport: true } },
  });
}

describe("MenuBar", () => {
  it("renders one button per menu inside a menubar", () => {
    const wrapper = open();
    expect(wrapper.get('[role="menubar"]').findAll("button").map((b) => b.text())).toEqual(["文件", "编辑"]);
    wrapper.unmount();
  });

  it("opens the clicked menu and reports aria-expanded", async () => {
    const wrapper = open();
    const fileButton = wrapper.findAll('[role="menubar"] button')[0];
    expect(wrapper.find('[role="menu"]').exists()).toBe(false);
    await fileButton.trigger("click");
    await nextTick();
    await nextTick();
    expect(fileButton.attributes("aria-expanded")).toBe("true");
    expect(wrapper.find('[role="menu"]').exists()).toBe(true);
    expect(wrapper.find('[role="menu"]').attributes("aria-label")).toContain("文件");
    wrapper.unmount();
  });

  it("emits select with menu id and item id", async () => {
    const wrapper = open();
    await wrapper.findAll('[role="menubar"] button')[0].trigger("click");
    await nextTick();
    await nextTick();
    await wrapper.get('[data-id="new-query"]').trigger("click");
    expect(wrapper.emitted("select")?.[0]).toEqual(["file", "new-query"]);
    expect(wrapper.find('[role="menu"]').exists()).toBe(false);
    wrapper.unmount();
  });

  it("closes when the same button is clicked again (pointerdown then click)", async () => {
    const wrapper = mount(MenuBar, {
      props: { menus: MENUS },
      global: { stubs: { teleport: true } },
      attachTo: document.body,
    });
    const button = wrapper.findAll('[role="menubar"] button')[0];
    await button.trigger("click");
    await nextTick();
    await nextTick();
    expect(button.attributes("aria-expanded")).toBe("true");
    // 真实浏览器序列：先 pointerdown 再 click
    button.element.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, cancelable: true }));
    await nextTick();
    // 触发器在排除列表里，pointerdown 不应关掉菜单
    expect(button.attributes("aria-expanded")).toBe("true");
    button.element.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await nextTick();
    await nextTick();
    // 这一次 click 应当把它关掉，而不是重开
    expect(button.attributes("aria-expanded")).toBe("false");
    expect(wrapper.find('[role="menu"]').exists()).toBe(false);
    wrapper.unmount();
  });

  it("switches menus when another menu button is clicked and closes on Escape", async () => {
    const wrapper = open();
    const [fileButton, editButton] = wrapper.findAll('[role="menubar"] button');
    await fileButton.trigger("click");
    await nextTick();
    await nextTick();
    expect(fileButton.attributes("aria-expanded")).toBe("true");
    await editButton.trigger("click");
    await nextTick();
    await nextTick();
    expect(fileButton.attributes("aria-expanded")).toBe("false");
    expect(editButton.attributes("aria-expanded")).toBe("true");
    await wrapper.get('[role="menu"]').trigger("keydown", { key: "Escape" });
    expect(editButton.attributes("aria-expanded")).toBe("false");
    wrapper.unmount();
  });

  it("moves focus between menu buttons with arrow keys", async () => {
    const wrapper = open();
    const buttons = wrapper.findAll('[role="menubar"] button');
    await buttons[0].element.focus();
    await buttons[0].trigger("keydown", { key: "ArrowRight" });
    expect(document.activeElement?.textContent).toBe("编辑");
    await buttons[1].trigger("keydown", { key: "ArrowLeft" });
    expect(document.activeElement?.textContent).toBe("文件");
    wrapper.unmount();
  });
});
