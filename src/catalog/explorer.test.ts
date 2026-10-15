import { mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { nextTick } from "vue";
import { setFocusDatabase, setListConnections, setOpenConnection } from "./backend";
import type { SavedConnection } from "../connections/model";
import ExplorerTree from "./ExplorerTree.vue";
import { catalogFixture, createRefreshFixture, pageArchive, pagedCatalog } from "./fixtures";
import { refreshCatalog, replaceCatalog, resetCatalog, setObjectPage, setPageSource, setRefreshSource } from "./store";
import type { CatalogConnection } from "./types";

afterEach(() => {
  resetCatalog();
  setFocusDatabase(null);
  setListConnections(null);
  setOpenConnection(null);
});

beforeEach(() => {
  setObjectPage(100);
});

function savedConnection(id: string, passwordSaved: boolean): SavedConnection {
  return {
    id,
    name: id,
    engine: "mariadb",
    env: "prod",
    host: "127.0.0.1",
    port: 3307,
    database: "analytics",
    username: "rillsql",
    filePath: null,
    tls: null,
    ssh: null,
    passwordSaved,
    sshPasswordSaved: false,
    sshHostKey: null,
  };
}

function names(wrapper: ReturnType<typeof mount>, selector: string): string[] {
  return wrapper.findAll(selector).map((node) => node.attributes("data-object") ?? "");
}

describe("explorer tree", () => {
  it("shows engine levels, a plain failed-read note with retry, the sqlite foreign key state, and the next batch", async () => {
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
    expect(postgres.find(".current-db").attributes("title")).toBe("当前会话连接的数据库");
    const failed = postgres.get("[data-schema-error]");
    expect(failed.text()).toContain("这部分对象列表没读出来");
    expect(failed.text()).toContain("没有权限读取 restricted 里的对象");
    expect(failed.get('[data-action="tree-retry"]').text()).toBe("重试");
    expect(postgres.text()).not.toContain("里没有表或视图");

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
    expect(sqlite.get("[data-foreign-keys]").text()).toContain("已关闭");
    expect(sqlite.get("[data-foreign-keys]").text()).toContain("foreign_keys = OFF");
    expect(names(sqlite, "[data-level=object]")).toEqual(["notes", "v_note_summary"]);

    expect(names(wrapper, "[data-schema=archive][data-level=object]")).toHaveLength(100);
    const more = wrapper.get("[data-more]");
    expect(more.text()).toContain("已显示 100 / 120 个");
    await more.get('[data-action="tree-more"]').trigger("click");
    expect(names(wrapper, "[data-schema=archive][data-level=object]")).toHaveLength(120);
    wrapper.unmount();
  });

  it("keeps same-named tables distinguishable in filter results with full paths", async () => {
    replaceCatalog(catalogFixture());
    const wrapper = mount(ExplorerTree, { props: { filter: "customers" } });

    expect(wrapper.text()).toContain("找到 2 个名称包含“customers”的对象");
    const hits = wrapper.findAll("[data-hit]");
    expect(hits.map((node) => node.attributes("data-path"))).toEqual(["Atlas / atlas_dev / public", "Atlas / atlas_dev / analytics"]);
    for (const hit of hits) expect(hit.get(".hit-path").text()).toContain(hit.attributes("data-path") ?? "");
    expect(wrapper.get("[data-skipped]").text()).toContain("Analytics 未连接，没有参与筛选");
    expect(wrapper.text()).not.toContain("events");

    await wrapper.setProps({ filter: "zzz" });
    expect(wrapper.text()).toContain("已连接的数据库里没有名称包含“zzz”的表或视图");
    wrapper.unmount();
  });

  it("connects from the disconnected note when the password is already saved", async () => {
    replaceCatalog(catalogFixture().filter((item) => item.id === "analytics"));
    const opened: string[] = [];
    setListConnections(async () => [savedConnection("analytics", true)]);
    setOpenConnection(async (id) => {
      opened.push(id);
      return { connected: true, report: { status: "ok", stages: [] } };
    });
    setRefreshSource(() =>
      catalogFixture()
        .filter((item) => item.id === "analytics")
        .map((item) => ({ ...item, status: "connected" as const })),
    );
    const wrapper = mount(ExplorerTree, { props: { filter: "" } });
    expect(wrapper.get("[data-connection=analytics] .conn-state").text()).toContain("未连接");
    expect(wrapper.get("[data-action=tree-connect]").text()).toBe("连接");
    await wrapper.get("[data-connection=analytics] .tw").trigger("click");
    expect(wrapper.get("[data-disconnected]").text()).toContain("连接后才会读取这里的表");
    expect(opened).toEqual([]);
    await wrapper.get("[data-connection=analytics] .tr-conn").trigger("click");
    expect(opened).toEqual(["analytics"]);
    expect(wrapper.text()).toContain("events");
    wrapper.unmount();
  });

  it("asks for the database password when it is not saved", async () => {
    replaceCatalog(catalogFixture().filter((item) => item.id === "analytics"));
    let opened = false;
    setListConnections(async () => [savedConnection("analytics", false)]);
    setOpenConnection(async () => {
      opened = true;
      return { connected: true, report: { status: "ok", stages: [] } };
    });
    const wrapper = mount(ExplorerTree, { props: { filter: "" } });
    await wrapper.get("[data-connection=analytics] .tr-conn").trigger("click");
    expect(opened).toBe(false);
    expect(wrapper.get("input[aria-label='数据库密码']").exists()).toBe(true);
    wrapper.unmount();
  });

  it("connects the postgres session to another database instead of showing a failed list", async () => {
    const attached: CatalogConnection = {
      id: "local",
      name: "127.0.0.1",
      engine: "postgres",
      env: "dev",
      envLabel: "开发",
      envTone: "dev",
      status: "connected",
      defaultDatabase: "postgres",
      foreignKeys: null,
      error: null,
      databases: [
        { name: "postgres", error: null, schemas: [{ name: "public", error: null, total: 0, objects: [] }] },
        {
          name: "rillsql",
          error: null,
          schemas: [{ name: "public", error: null, total: 1, objects: [{ name: "customers", kind: "table", estimatedRows: 2, stale: false }] }],
        },
      ],
    };
    replaceCatalog([
      {
        ...attached,
        databases: [
          attached.databases[0]!,
          { name: "rillsql", error: "当前会话连在 postgres，没有另开连接读取 rillsql", schemas: [] },
        ],
      },
    ]);
    const seen: string[] = [];
    setFocusDatabase(async (id, database) => {
      seen.push(`${id}:${database}`);
    });
    setRefreshSource(() => [attached]);
    const wrapper = mount(ExplorerTree, { props: { filter: "" } });
    const note = wrapper.get("[data-other-database]");
    expect(note.text()).toContain("这条会话现在连在 postgres");
    expect(note.text()).toContain("连接到 rillsql");
    expect(note.text()).not.toContain("对象列表没读出来");
    await note.get("[data-action=focus-database]").trigger("click");
    expect(seen).toEqual(["local:rillsql"]);
    expect(wrapper.emitted("focused-database")?.[0]).toEqual(["local", "rillsql"]);
    expect(wrapper.text()).toContain("customers");
    wrapper.unmount();
  });

  it("opens a table on left click and uses an app menu on right click", async () => {
    const listed = catalogFixture();
    const customers = listed[0]?.databases[0]?.schemas[0]?.objects[0];
    if (customers) customers.estimatedRows = -1;
    const orders = listed[0]?.databases[0]?.schemas[0]?.objects.find((object) => object.name === "orders");
    if (orders) orders.estimatedRows = 0;
    replaceCatalog(listed);
    const wrapper = mount(ExplorerTree, { props: { filter: "" }, attachTo: document.body });
    const row = wrapper.get("[data-database=atlas_dev][data-schema=public][data-object=customers]");
    expect(row.text()).not.toContain("-1");
    expect(wrapper.get("[data-database=atlas_dev][data-schema=public][data-object=orders]").text()).toContain("0");
    await row.trigger("click");
    expect(wrapper.emitted("open-object")?.[0]?.[0]).toMatchObject({
      name: "customers",
      database: "atlas_dev",
      schema: "public",
      kind: "table",
      engine: "postgres",
    });
    expect(wrapper.emitted("open-object")?.[0]?.[1]).toBe("data");

    const event = new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 24, clientY: 36 });
    row.element.dispatchEvent(event);
    await nextTick();
    expect(event.defaultPrevented).toBe(true);
    const menu = document.body.querySelector("[data-menu=object]");
    expect(menu?.textContent).toContain("打开数据");
    expect(menu?.textContent).toContain("查看结构");
    expect(menu?.textContent).toContain("新建查询");
    expect(menu?.textContent).toContain("删除表");
    (menu?.querySelector("[data-action=open-structure]") as HTMLButtonElement | null)?.click();
    await nextTick();
    expect(wrapper.emitted("open-object")?.[1]?.[1]).toBe("structure");
    expect(wrapper.emitted("open-object")?.[1]?.[2]).toBe("columns");

    const view = wrapper.get("[data-object=v_note_summary]");
    view.element.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 12, clientY: 18 }));
    await nextTick();
    expect(document.body.querySelector("[data-action=drop-table]")).toBeNull();
    expect(document.body.querySelector("[data-action=open-data]")?.textContent).toContain("打开数据");
    wrapper.unmount();
  });

  it("marks an externally deleted object stale in plain words and removes it on the next refresh", async () => {
    const fixture = createRefreshFixture();
    fixture.hide("atlas", "atlas_dev", "public", "categories");
    setRefreshSource(() => fixture.snapshot());
    replaceCatalog(catalogFixture());
    const wrapper = mount(ExplorerTree, { props: { filter: "" } });

    expect(wrapper.get('[data-schema=public][data-object=categories]').attributes("data-stale")).toBe("false");
    await wrapper.get('[data-action="tree-refresh"]').trigger("click");
    const stale = wrapper.get('[data-schema=public][data-object=categories]');
    expect(stale.attributes("data-stale")).toBe("true");
    expect(stale.text()).toContain("已失效");
    expect(stale.attributes("title")).toContain("已在外部被删除");
    expect(wrapper.get('[data-schema=public][data-object=orders]').attributes("data-stale")).toBe("false");
    await stale.trigger("click");
    expect(wrapper.emitted("open-object")).toBeUndefined();

    refreshCatalog();
    await wrapper.vm.$nextTick();
    expect(wrapper.find('[data-schema=public][data-object=categories]').exists()).toBe(false);
    expect(wrapper.get('[data-schema=public][data-object=orders]').attributes("data-stale")).toBe("false");
    expect(wrapper.get('[data-schema=analytics][data-object=customers]').attributes("data-stale")).toBe("false");
    wrapper.unmount();
  });

  it("shows a retryable read failure with its reason instead of an empty database", () => {
    const [postgres] = catalogFixture();
    replaceCatalog([{ ...postgres, databases: [], error: "读取库表失败：permission denied" }]);
    const wrapper = mount(ExplorerTree, { props: { filter: "" } });
    const note = wrapper.get("[data-connection-error]");
    expect(note.text()).toContain("对象列表没读出来");
    expect(note.text()).toContain("permission denied");
    expect(note.get('[data-action="tree-retry"]').text()).toBe("重试");
    expect(wrapper.text()).not.toContain("里没有表或视图");
    expect(wrapper.text()).not.toContain("还没有打开的连接");
    wrapper.unmount();
  });
});
