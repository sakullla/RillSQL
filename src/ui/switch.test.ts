import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import Switch from "./Switch.vue";

function mountSwitch(modelValue: boolean, disabled = false) {
  return mount(Switch, {
    props: { modelValue, ariaLabel: "自动保存", disabled },
    attachTo: document.body,
  });
}

describe("Switch", () => {
  it("exposes role=switch and reflects modelValue in aria-checked", () => {
    const off = mountSwitch(false);
    const on = mountSwitch(true);
    expect(off.get('[role="switch"]').attributes("aria-checked")).toBe("false");
    expect(on.get('[role="switch"]').attributes("aria-checked")).toBe("true");
    off.unmount();
    on.unmount();
  });

  it("emits the inverted value on click", async () => {
    const wrapper = mountSwitch(false);
    await wrapper.get('[role="switch"]').trigger("click");
    expect(wrapper.emitted("update:modelValue")?.[0]).toEqual([true]);
    wrapper.unmount();
  });

  it("does not emit when disabled", async () => {
    const wrapper = mountSwitch(true, true);
    await wrapper.get('[role="switch"]').trigger("click");
    expect(wrapper.emitted("update:modelValue")).toBeUndefined();
    wrapper.unmount();
  });
});
