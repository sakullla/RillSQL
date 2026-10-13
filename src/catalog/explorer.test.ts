import { mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import ExplorerTree from "./ExplorerTree.vue";
import { catalogFixture, createRefreshFixture, pageArchive, pagedCatalog } from "./fixtures";
import { refreshCatalog, replaceCatalog, resetCatalog, setObjectPage, setPageSource, setRefreshSource } from "./store";

afterEach(() => {
  resetCatalog();
});

beforeEach(() => {
  setObjectPage(100);
});

function names(wrapper: ReturnType<typeof mount>, selector: string): string[] {
  return wrapper.findAll(selector).map((node) => node.attributes("data-object") ?? "");
}

describe("explorer tree", () => {
  it("shows engine levels, paths, a failed read, the sqlite foreign key switch, and the next batch", async () => {
    setPageSource(pageArchive);
    replaceCatalog(pagedCatalog());
    const wrapper = mount(ExplorerTree, { props: { filter: "" } });

    const postgres = wrapper.get("[data-engine=postgres]");
    expect(postgres.findAll("[data-level=database]").map((node) => node.attributes("data-database"))).toEqual(["atlas_dev", "atlas_audit"]);
    expect(postgres.findAll("[data-level=schema]").map((node) => node.attributes("data-schema"))).toEqual([
      "public",
      "analytics",
      "restricted",
      "archive",
      "public",
    ]);
    expect(postgres.text()).toContain("无法读取对象列表");
    expect(postgres.text()).toContain("permission denied for schema restricted");
    expect(postgres.text()).not.toContain("没有表");

    expect(wrapper.get("[data-engine=mysql]").findAll("[data-level=schema]")).toHaveLength(0);
    expect(wrapper.get("[data-engine=mysql]").text()).toContain("shop_staging");
    expect(names(wrapper, "[data-engine=mysql] [data-level=object]")).toEqual(["products", "orders"]);

    const mariadb = wrapper.findAll("[data-engine=mariadb]");
    expect(mariadb).toHaveLength(2);
    expect(mariadb[0].findAll("[data-level=schema]")).toHaveLength(0);
    expect(mariadb[0].text()).toContain("ledger");
    expect(mariadb[0].text()).toContain("entries");
    expect(mariadb[1].text()).toContain("未连接");
    expect(mariadb[1].text()).not.toContain("events");

    const sqlite = wrapper.get("[data-engine=sqlite]");
    expect(sqlite.find("[data-level=database]").exists()).toBe(false);
    expect(sqlite.find("[data-level=schema]").exists()).toBe(false);
    expect(sqlite.get("[data-foreign-keys]").text()).toContain("foreign_keys = OFF");
    expect(names(sqlite, "[data-level=object]")).toEqual(["notes", "v_note_summary"]);

    expect(names(wrapper, "[data-schema=archive][data-level=object]")).toHaveLength(100);
    expect(wrapper.text()).toContain("已显示 100 / 120 个对象");
    await wrapper.get('[data-action="tree-more"]').trigger("click");
    expect(names(wrapper, "[data-schema=archive][data-level=object]")).toHaveLength(120);

    await wrapper.setProps({ filter: "customers" });
    const hits = wrapper.findAll("[data-hit]");
    expect(hits.map((node) => node.attributes("data-path"))).toEqual(["Atlas / atlas_dev / public", "Atlas / atlas_dev / analytics"]);
    expect(wrapper.get("[data-skipped]").text()).toContain("Analytics 未连接，未包含在结果中");
    expect(wrapper.text()).not.toContain("events");
  });

  it("marks an externally deleted object stale and removes it on the next refresh", async () => {
    const fixture = createRefreshFixture();
    fixture.hide("atlas", "atlas_dev", "public", "categories");
    setRefreshSource(() => fixture.snapshot());
    replaceCatalog(catalogFixture());
    const wrapper = mount(ExplorerTree, { props: { filter: "" } });

    expect(wrapper.get('[data-schema=public][data-object=categories]').attributes("data-stale")).toBe("false");
    await wrapper.get('[data-action="tree-refresh"]').trigger("click");
    expect(wrapper.get('[data-schema=public][data-object=categories]').attributes("data-stale")).toBe("true");
    expect(wrapper.get('[data-schema=public][data-object=categories]').text()).toContain("已失效");
    expect(wrapper.get('[data-schema=public][data-object=orders]').attributes("data-stale")).toBe("false");

    refreshCatalog();
    await wrapper.vm.$nextTick();
    expect(wrapper.find('[data-schema=public][data-object=categories]').exists()).toBe(false);
    expect(wrapper.get('[data-schema=public][data-object=orders]').attributes("data-stale")).toBe("false");
    expect(wrapper.get('[data-schema=analytics][data-object=customers]').attributes("data-stale")).toBe("false");
  });

  it("shows a retryable connection error instead of an empty database", () => {
    const [postgres] = catalogFixture();
    replaceCatalog([{ ...postgres, databases: [], error: "读取库表失败：permission denied" }]);
    const wrapper = mount(ExplorerTree, { props: { filter: "" } });
    expect(wrapper.get("[data-connection-error]").text()).toContain("无法读取对象列表");
    expect(wrapper.get("[data-connection-error]").text()).toContain("permission denied");
    expect(wrapper.text()).not.toContain("没有表");
    expect(wrapper.text()).not.toContain("还没有连接");
  });
});
