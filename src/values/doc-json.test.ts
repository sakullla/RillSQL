import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import DocJsonView from "./DocJsonView.vue";

describe("document json viewer (values family)", () => {
  it("pretty prints a mongo document with extended json values intact", () => {
    const json = '{"_id":{"$oid":"65f2c1e4a1b2c3d4e5f60718"},"客户":"林夏","标签":["加急","普通"],"合计":128.5}';
    const wrapper = mount(DocJsonView, { props: { json } });
    expect(wrapper.find("[data-value-kind=json]").exists()).toBe(true);
    const text = wrapper.get("pre").text();
    expect(text).toContain('"$oid": "65f2c1e4a1b2c3d4e5f60718"');
    expect(text).toContain("林夏");
    expect(text).toContain("加急");
    expect(text.split("\n").length).toBeGreaterThan(3);
  });

  it("falls back to raw text and an explicit note when the json is malformed", () => {
    const wrapper = mount(DocJsonView, { props: { json: "{broken" } });
    expect(wrapper.find(".dj-invalid").exists()).toBe(true);
    expect(wrapper.text()).toContain("{broken");
  });
});
