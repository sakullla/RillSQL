import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import QueryResultGrid from "./QueryResultGrid.vue";
import { customersFixture } from "./fixture";
import { rowWindow } from "./virtual";

describe("query result window", () => {
  it("bounds DOM work independently of the result length, including the final row", () => {
    const count = 100_000;
    for (const top of [0, 3200, count * 32 - 400]) {
      const range = rowWindow(count, top, 400, 32);
      expect(range.end - range.start).toBeLessThan(30);
      expect(range.top + (range.end - range.start) * 32 + range.bottom).toBe(count * 32);
    }
    expect(rowWindow(count, count * 32 - 400, 400, 32).end).toBe(count);
    expect(rowWindow(0, 0, 400, 32)).toEqual({ start: 0, end: 0, top: 0, bottom: 0 });
  });

  it("filters the entire loaded result even when most rows are not rendered", async () => {
    const data = customersFixture(500);
    data.rows[499]!.cells.name = "needle-outside-viewport";
    const wrapper = mount(QueryResultGrid, { props: { tabId: "virtual", statement: "SELECT * FROM customers", columns: data.schema.columns, rows: data.rows, truncated: false } });
    expect(wrapper.findAll("[data-row-id]").length).toBeLessThan(30);
    expect(wrapper.text()).not.toContain("needle-outside-viewport");
    await wrapper.get("input[aria-label='在已读取行中查找']").setValue("needle-outside-viewport");
    expect(wrapper.findAll("[data-row-id]")).toHaveLength(1);
    expect(wrapper.text()).toContain("needle-outside-viewport");
    wrapper.unmount();
  });
});
