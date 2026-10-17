import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import { nextTick } from "vue";
import Listbox from "./Listbox.vue";
import type { ListboxOption } from "./types";

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
});
