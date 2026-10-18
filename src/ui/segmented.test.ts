import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import Segmented from "./Segmented.vue";
import type { SegmentedOption } from "./types";

const OPTIONS: SegmentedOption[] = [
  { value: "comfortable", label: "舒适" },
  { value: "compact", label: "紧凑" },
];

function mountSegmented(modelValue: string) {
  return mount(Segmented, {
    props: { modelValue, options: OPTIONS, ariaLabel: "行密度" },
    attachTo: document.body,
  });
}

describe("Segmented", () => {
  it("renders a radiogroup with the current option checked", () => {
    const wrapper = mountSegmented("compact");
    expect(wrapper.get('[role="radiogroup"]').attributes("aria-label")).toBe("行密度");
    const radios = wrapper.findAll('[role="radio"]');
    expect(radios).toHaveLength(2);
    expect(radios[0].attributes("aria-checked")).toBe("false");
    expect(radios[1].attributes("aria-checked")).toBe("true");
  });

  it("emits the clicked option value", async () => {
    const wrapper = mountSegmented("comfortable");
    await wrapper.findAll('[role="radio"]')[1].trigger("click");
    expect(wrapper.emitted("update:modelValue")?.[0]).toEqual(["compact"]);
    wrapper.unmount();
  });
});
