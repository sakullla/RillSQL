import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import { nextTick } from "vue";
import Listbox from "./Listbox.vue";
import type { ListboxOption } from "./types";

const source = readFileSync(resolve(dirname(fileURLToPath(import.meta.url)), "Listbox.vue"), "utf8");

const OPTIONS: ListboxOption[] = [
  { value: "postgres", label: "PostgreSQL" },
  { value: "mysql", label: "MySQL" },
  { value: "mariadb", label: "MariaDB", disabled: true },
  { value: "sqlite", label: "SQLite" },
];

function open() {
  const wrapper = mount(Listbox, {
    props: { modelValue: null, options: OPTIONS, ariaLabel: "数据库类型" },
    attachTo: document.body,
  });
  return wrapper;
}

describe("Listbox", () => {
  it("starts closed and reports the selected label", async () => {
    const wrapper = mount(Listbox, { props: { modelValue: "mysql", options: OPTIONS, ariaLabel: "数据库类型" } });
    const trigger = wrapper.get('[role="combobox"]');
    expect(trigger.attributes("aria-expanded")).toBe("false");
    expect(trigger.text()).toBe("MySQL");
    expect(wrapper.findAll('[role="option"]')).toHaveLength(0);
  });

  it("opens on click and lists every option", async () => {
    const wrapper = open();
    await wrapper.get('[role="combobox"]').trigger("click");
    expect(wrapper.get('[role="combobox"]').attributes("aria-expanded")).toBe("true");
    expect(wrapper.findAll('[role="option"]').map((n) => n.text())).toContain("SQLite");
    wrapper.unmount();
  });

  it("moves with arrows and selects with Enter, skipping disabled options", async () => {
    const wrapper = open();
    await wrapper.get('[role="combobox"]').trigger("click");
    const list = wrapper.get('[role="listbox"]');
    await list.trigger("keydown", { key: "ArrowDown" });
    await list.trigger("keydown", { key: "ArrowDown" });
    // 第三项 MariaDB 是 disabled，光标应跳到 SQLite
    expect(wrapper.findAll(".listbox-option.on").at(0)?.text()).toContain("SQLite");
    await list.trigger("keydown", { key: "Enter" });
    expect(wrapper.emitted("update:modelValue")?.[0]).toEqual(["sqlite"]);
    expect(wrapper.get('[role="combobox"]').attributes("aria-expanded")).toBe("false");
    wrapper.unmount();
  });

  it("closes on Escape without emitting", async () => {
    const wrapper = open();
    await wrapper.get('[role="combobox"]').trigger("click");
    await wrapper.get('[role="listbox"]').trigger("keydown", { key: "Escape" });
    expect(wrapper.find('[role="listbox"]').exists()).toBe(false);
    expect(wrapper.emitted("update:modelValue")).toBeUndefined();
    wrapper.unmount();
  });

  it("closes when a pointer lands outside", async () => {
    const wrapper = open();
    await wrapper.get('[role="combobox"]').trigger("click");
    document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    await nextTick();
    expect(wrapper.find('[role="listbox"]').exists()).toBe(false);
    wrapper.unmount();
  });

  it("filters options when filterable", async () => {
    const wrapper = mount(Listbox, {
      props: { modelValue: null, options: OPTIONS, filterable: true, ariaLabel: "数据库类型" },
      attachTo: document.body,
    });
    await wrapper.get('[role="combobox"]').trigger("click");
    await wrapper.get(".listbox-filter").setValue("sqlit");
    expect(wrapper.findAll('[role="option"]').map((n) => n.text())).toEqual(["SQLite"]);
    wrapper.unmount();
  });

  it("flips the popup above the trigger when it would overflow the viewport", async () => {
    const wrapper = open();
    const el = wrapper.get('[role="combobox"]').element as HTMLElement;
    el.getBoundingClientRect = () =>
      ({ top: window.innerHeight - 40, bottom: window.innerHeight - 10, left: 20, right: 220, width: 200, height: 30, x: 20, y: window.innerHeight - 40, toJSON: () => ({}) }) as DOMRect;
    await wrapper.get('[role="combobox"]').trigger("click");
    expect(wrapper.find(".listbox-pop").classes()).toContain("up");
    wrapper.unmount();
  });

  it("keeps the popup below the trigger when there is room", async () => {
    const wrapper = open();
    const el = wrapper.get('[role="combobox"]').element as HTMLElement;
    el.getBoundingClientRect = () =>
      ({ top: 40, bottom: 70, left: 20, right: 220, width: 200, height: 30, x: 20, y: 40, toJSON: () => ({}) }) as DOMRect;
    await wrapper.get('[role="combobox"]').trigger("click");
    expect(wrapper.find(".listbox-pop").classes()).not.toContain("up");
    wrapper.unmount();
  });

  it("exposes the active option to assistive tech via aria-activedescendant", async () => {
    const wrapper = open();
    await wrapper.get('[role="combobox"]').trigger("click");
    const options = () => wrapper.findAll('[role="option"]');
    expect(wrapper.get('[role="listbox"]').attributes("aria-activedescendant")).toBe(options()[0].attributes("id"));
    await wrapper.get('[role="listbox"]').trigger("keydown", { key: "ArrowDown" });
    expect(wrapper.get('[role="listbox"]').attributes("aria-activedescendant")).toBe(options()[1].attributes("id"));
    // 筛选模式下焦点在筛选框上，同样要能读到当前项
    const filtered = mount(Listbox, {
      props: { modelValue: null, options: OPTIONS, filterable: true, ariaLabel: "数据库类型" },
      attachTo: document.body,
    });
    await filtered.get('[role="combobox"]').trigger("click");
    const input = filtered.get(".listbox-filter");
    expect(input.attributes("aria-activedescendant")).toBe(filtered.findAll('[role="option"]')[0].attributes("id"));
    filtered.unmount();
    wrapper.unmount();
  });

  it("does not close when the pointer lands on the popup chrome", async () => {
    const wrapper = mount(Listbox, {
      props: { modelValue: null, options: OPTIONS, filterable: true, ariaLabel: "数据库类型" },
      attachTo: document.body,
    });
    await wrapper.get('[role="combobox"]').trigger("click");
    // 弹层根节点上的 pointerdown 不应关闭（点在 padding / 筛选框下方空白处）
    wrapper.get(".listbox-pop").element.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    await nextTick();
    expect(wrapper.find('[role="listbox"]').exists()).toBe(true);
    wrapper.unmount();
  });

  it("Home and End land on enabled options only", async () => {
    const firstDisabled = mount(Listbox, {
      props: {
        modelValue: null,
        options: [
          { value: "a", label: "A", disabled: true },
          { value: "b", label: "B" },
          { value: "c", label: "C" },
        ],
        ariaLabel: "选项",
      },
      attachTo: document.body,
    });
    await firstDisabled.get('[role="combobox"]').trigger("click");
    await firstDisabled.get('[role="listbox"]').trigger("keydown", { key: "Home" });
    expect(firstDisabled.findAll(".listbox-option.on").at(0)?.text()).toBe("B");
    firstDisabled.unmount();

    const lastDisabled = mount(Listbox, {
      props: {
        modelValue: null,
        options: [
          { value: "a", label: "A" },
          { value: "b", label: "B" },
          { value: "c", label: "C", disabled: true },
        ],
        ariaLabel: "选项",
      },
      attachTo: document.body,
    });
    await lastDisabled.get('[role="combobox"]').trigger("click");
    await lastDisabled.get('[role="listbox"]').trigger("keydown", { key: "End" });
    expect(lastDisabled.findAll(".listbox-option.on").at(0)?.text()).toBe("B");
    lastDisabled.unmount();
  });

  it("does not open or change its value while disabled", async () => {
    const wrapper = mount(Listbox, {
      props: { modelValue: "mysql", options: OPTIONS, ariaLabel: "数据库类型", disabled: true },
      attachTo: document.body,
    });
    const trigger = wrapper.get('[role="combobox"]');
    expect(trigger.attributes("disabled")).toBe("");
    expect(wrapper.classes()).toContain("disabled");
    await trigger.trigger("click");
    await trigger.trigger("keydown", { key: "ArrowDown" });
    await trigger.trigger("keydown", { key: "Enter" });
    await trigger.trigger("keydown", { key: " " });
    expect(wrapper.find('[role="listbox"]').exists()).toBe(false);
    expect(wrapper.emitted("update:modelValue")).toBeUndefined();
    expect(trigger.text()).toBe("MySQL");
    wrapper.unmount();
  });

  it("closes an open list when it becomes disabled", async () => {
    const wrapper = open();
    await wrapper.get('[role="combobox"]').trigger("click");
    expect(wrapper.find('[role="listbox"]').exists()).toBe(true);
    await wrapper.setProps({ disabled: true });
    expect(wrapper.find('[role="listbox"]').exists()).toBe(false);
    await wrapper.get('[role="combobox"]').trigger("click");
    const option = wrapper.findAll('[role="option"]').find((node) => node.text() === "PostgreSQL");
    if (option) await option.trigger("click");
    expect(wrapper.emitted("update:modelValue")).toBeUndefined();
    wrapper.unmount();
  });

  it("keeps hover, press, focus and disabled visible on the trigger", () => {
    expect(source).toContain(".listbox-trigger:hover:not(:disabled) { background: var(--hover); }");
    expect(source).toContain(".listbox-trigger:active:not(:disabled) { background: var(--active); box-shadow: inset 0 1px 2px var(--press); transform: translateY(.5px); }");
    expect(source).toContain(".listbox-trigger:focus-visible:not(:disabled) { outline: 2px solid var(--accent); outline-offset: 1px; }");
    expect(source).toContain(".listbox-trigger:disabled {");
    expect(source).toContain("background: var(--disabled-bg);");
    expect(source).toContain("color: var(--disabled-fg);");
  });
});
