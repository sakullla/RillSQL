import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import { resetCatalog } from "../catalog/store";
import AppShell from "../shell/AppShell.vue";
import Sidebar from "../shell/Sidebar.vue";
import { createSession, snapshotSession } from "../shell/session";
import WorkspaceView from "../shell/WorkspaceView.vue";
import type { Diagram } from "./types";
import DiagramView from "./DiagramView.vue";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke,
}));

const WRITE_COMMANDS = ["ddl_describe", "ddl_preview", "ddl_execute", "grid_commit", "query_start", "catalog_page", "catalog_definition"];

const linked: Diagram = {
  database: "bookstore",
  schema: "public",
  notice: null,
  tables: [
    { name: "customers", columns: ["id", "name"], outside: [] },
    {
      name: "orders",
      columns: ["id", "customer_id"],
      outside: ["外键 orders_account_fkey 指向 other.accounts(id)，不在当前范围"],
    },
    { name: "notes", columns: ["id", "body"], outside: [] },
  ],
  edges: [{ name: "orders_customer_fkey", fromTable: "orders", fromColumns: ["customer_id"], toTable: "customers", toColumns: ["id"] }],
  omitted: [{ name: "secrets", message: "没有读到表 secrets：permission denied" }],
};

afterEach(() => {
  location.hash = "";
  localStorage.clear();
  resetCatalog();
  invoke.mockReset();
});

describe("relationship diagram", () => {
  it("draws in-range tables and foreign keys without sending a write", async () => {
    invoke.mockResolvedValue(linked);
    const wrapper = mount(DiagramView, {
      props: { connectionId: "pg", database: "bookstore", schema: "public", label: "public 关系图" },
    });
    await flushPromises();
    expect(wrapper.get("[data-diagram]").attributes("data-readonly")).toBe("true");
    expect(wrapper.find("button").exists()).toBe(false);
    expect(wrapper.findAll("[data-table]").map((node) => node.attributes("data-table-name"))).toEqual(["customers", "orders", "notes"]);
    expect(wrapper.text()).not.toContain("v_orders");
    expect(wrapper.get("[data-table-name=customers]").text()).toContain("id");
    expect(wrapper.get("[data-table-name=customers]").text()).toContain("name");
    const edge = wrapper.get("[data-edge]");
    expect(edge.attributes("data-from")).toBe("orders");
    expect(edge.attributes("data-to")).toBe("customers");
    expect(edge.attributes("data-from-columns")).toBe("customer_id");
    expect(edge.attributes("data-to-columns")).toBe("id");
    expect(edge.text()).toContain("customer_id");
    expect(edge.text()).toContain("id");
    expect(wrapper.get("[data-omitted-table=secrets]").text()).toContain("secrets");
    expect(wrapper.find("[data-table-name=secrets]").exists()).toBe(false);
    expect(wrapper.get("[data-outside]").text()).toContain("other.accounts");
    expect(wrapper.get("[data-outside]").text()).toContain("不在当前范围");
    expect(wrapper.find("[data-table-name=accounts]").exists()).toBe(false);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("diagram_read", { connectionId: "pg", database: "bookstore", schema: "public" });
    for (const command of WRITE_COMMANDS) {
      expect(invoke.mock.calls.some((call) => call[0] === command)).toBe(false);
    }
    wrapper.unmount();
  });

  it("explains an empty range and a refused schema without claiming the database changed", async () => {
    invoke.mockResolvedValue({ database: "shop", schema: "", tables: [], edges: [], omitted: [], notice: "当前范围没有表" });
    const empty = mount(DiagramView, { props: { connectionId: "shop", database: "shop", schema: "", label: "shop 关系图" } });
    await flushPromises();
    expect(empty.get("[data-diagram-notice]").text()).toBe("当前范围没有表");
    expect(empty.find("[data-table]").exists()).toBe(false);
    empty.unmount();

    invoke.mockReset();
    invoke.mockRejectedValue("PostgreSQL 关系图需要一个 schema。");
    const refused = mount(DiagramView, { props: { connectionId: "pg", database: "bookstore", schema: "", label: "关系图" } });
    await flushPromises();
    expect(refused.get("[data-diagram-error]").text()).toContain("需要一个 schema");
    expect(refused.find("[data-diagram-notice]").exists()).toBe(false);
    expect(invoke.mock.calls.some((call) => WRITE_COMMANDS.includes(String(call[0])))).toBe(false);
    refused.unmount();
  });

  it("opens one read-only diagram tab from the current range and does not keep it in the saved session", async () => {
    invoke.mockImplementation(async (command: string) => {
      if (command === "diagram_read") return linked;
      return [];
    });
    const wrapper = mount(AppShell);
    await flushPromises();
    wrapper.getComponent(Sidebar).vm.$emit("open-diagram", {
      connectionId: "pg",
      connectionName: "书店",
      database: "bookstore",
      engine: "postgres",
      schema: "public",
      path: "书店 / bookstore / public",
      label: "public 关系图",
    });
    await flushPromises();
    expect(wrapper.get("[data-diagram]").attributes("data-readonly")).toBe("true");
    expect(wrapper.get("[data-edge]").attributes("data-from-columns")).toBe("customer_id");
    expect(wrapper.get("[data-edge]").attributes("data-to-columns")).toBe("id");
    const tabs = wrapper.getComponent(WorkspaceView).props("tabs") as { id: string; kind: string }[];
    expect(tabs.filter((tab) => tab.kind === "diagram")).toHaveLength(1);
    expect(invoke.mock.calls.filter((call) => call[0] === "diagram_read")).toHaveLength(1);
    for (const command of WRITE_COMMANDS) {
      expect(invoke.mock.calls.some((call) => call[0] === command)).toBe(false);
    }
    wrapper.unmount();

    const session = createSession();
    const target = {
      id: "pg:bookstore:public",
      label: "public 关系图",
      path: "书店 / bookstore / public",
      connectionId: "pg",
      connectionName: "书店",
      database: "bookstore",
      engine: "postgres",
      status: "connected" as const,
      schema: "public",
    };
    session.openDiagram(target);
    session.openDiagram(target);
    expect(session.tabs.value.filter((tab) => tab.kind === "diagram")).toHaveLength(1);
    expect(session.activeTab.value?.diagram).toEqual({ schema: "public" });
    expect(snapshotSession(session.tabs.value, session.activeId.value).tabs).toEqual([]);
  });
});
