import { EditorView } from "@codemirror/view";
import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import { replaceCatalog, resetCatalog } from "../catalog/store";
import type { CatalogConnection } from "../catalog/types";
import { exportTasks, resetExportTasks, taskStatusText, type ExportTask } from "../export/tasks";
import type { TableTarget } from "../grid/target";
import type { RunDto } from "../query/api";
import { adoptRun, resetQueryRuntime } from "../query/runtime";
import AppShell from "./AppShell.vue";
import type { ShellTab } from "./session";
import WorkspaceView from "./WorkspaceView.vue";

const invoke = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke,
}));

afterEach(() => {
  location.hash = "";
  localStorage.clear();
  resetCatalog();
  resetQueryRuntime();
  resetExportTasks();
  invoke.mockReset();
});

function savedConnection(): CatalogConnection {
  return {
    id: "pg",
    name: "书店",
    engine: "postgres",
    env: "dev",
    envLabel: "开发",
    envTone: "dev",
    status: "disconnected",
    defaultDatabase: "bookstore",
    foreignKeys: null,
    error: null,
    databases: [],
  };
}

const jumpTarget: TableTarget = {
  connectionId: "pg",
  database: "bookstore",
  schema: "public",
  table: "orders",
  engine: "postgres",
};

const jumpSql = 'SELECT *\nFROM "public"."customers"\nWHERE "id" = \'1\';';

function doneRun(): RunDto {
  return {
    id: "run-1",
    connectionId: "pg",
    connectionName: "书店",
    database: "bookstore",
    engine: "postgres",
    status: "done",
    cancel: "none",
    cancelMessage: "",
    viaSsh: false,
    freshChannel: false,
    statements: [],
    startedAt: "2026-10-20T00:00:00Z",
    elapsedMs: 4,
  };
}

function editorText(wrapper: VueWrapper): string {
  const view = EditorView.findFromDOM(wrapper.get(".cm-content").element as HTMLElement);
  expect(view).toBeTruthy();
  return view!.state.doc.toString();
}

describe("AppShell", () => {
  it("opens the welcome page with two primary paths and no login gate", () => {
    const wrapper = mount(AppShell);
    const text = wrapper.text();
    expect(text).toContain("澄库");
    expect(text).toContain("RillSQL");
    expect(wrapper.find(".welcome").exists()).toBe(true);
    expect(wrapper.find(".titlebar").exists()).toBe(true);
    expect(wrapper.find(".rail").exists()).toBe(true);
    expect(wrapper.find(".statusbar").exists()).toBe(true);

    // R2：首次启动直接呈现两条主路径，无需文档或快捷键
    const welcome = wrapper.get(".welcome");
    const paths = welcome.findAll(".desk-action");
    expect(paths).toHaveLength(2);
    expect(paths[0]!.text()).toContain("新建连接");
    expect(paths[1]!.text()).toContain("打开 SQLite 文件");
    const solid = welcome.findAll(".btn.primary");
    expect(solid).toHaveLength(1);
    expect(solid[0]!.text()).toContain("新建连接");
    expect(paths[1]!.classes()).not.toContain("primary");
    expect(text).toContain("配置留在本机");
    expect(text).toContain("密码进入系统凭据库");
    expect(text).not.toContain("从数据，到答案。");
    expect(text).not.toContain("SQL FIRST");
    expect(text).not.toContain("WORKSPACE");
    expect(wrapper.find(".desk-db").exists()).toBe(false);
    expect(text).not.toMatch(/云登录|请登录/);
    wrapper.unmount();
  });

  it("keeps new query as the only solid welcome action when a connection exists", () => {
    replaceCatalog([savedConnection()]);
    const wrapper = mount(AppShell);
    const welcome = wrapper.get(".welcome");
    const solid = welcome.findAll(".btn.primary");
    expect(solid).toHaveLength(1);
    expect(solid[0]!.text()).toContain("新建查询");
    expect(welcome.findAll(".desk-action")).toHaveLength(0);
    expect(welcome.text()).not.toContain("新建连接");
    expect(welcome.text()).not.toContain("打开 SQLite");
    wrapper.unmount();
  });

  it("keeps secondary entries out of the status bar by default", () => {
    const wrapper = mount(AppShell);
    const status = wrapper.get(".statusbar").text();
    expect(status).not.toContain("快捷键");
    expect(status).not.toContain("仅存本机");
    wrapper.unmount();
  });

  it("navigates to the connections page from the new-connection path", async () => {
    const wrapper = mount(AppShell);
    await wrapper.findAll(".desk-action")[0]!.trigger("click");
    expect(location.hash).toBe("#/connections");
    expect(wrapper.find(".workspace-view").attributes("style")).toContain("display: none");
    wrapper.unmount();
  });

  it("opens a query tab and runs the referenced-row select on the target connection", async () => {
    replaceCatalog([{ ...savedConnection(), status: "connected" }]);
    invoke.mockImplementation(async (command: string) => {
      if (command === "query_start") return doneRun();
      throw new Error(command);
    });
    const wrapper = mount(AppShell);
    await flushPromises();
    wrapper.findComponent(WorkspaceView).vm.$emit("jump-reference", jumpSql, "public.customers", jumpTarget);
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(wrapper.findAll(".tab")).toHaveLength(1);
    expect(wrapper.get(".tab").text()).toContain("查询 1");
    const start = invoke.mock.calls.find((call) => call[0] === "query_start");
    expect(start?.[1]).toMatchObject({
      request: {
        connectionId: "pg",
        connectionName: "书店",
        database: "bookstore",
        engine: "postgres",
        sql: jumpSql,
        limit: 100,
      },
    });
    wrapper.unmount();
  });

  it("keeps the referenced-row draft with a notice when the auto run is rejected", async () => {
    replaceCatalog([{ ...savedConnection(), status: "connected" }]);
    invoke.mockImplementation(async (command: string) => {
      if (command === "query_start") throw new Error("这个连接已有正在执行的查询。");
      throw new Error(command);
    });
    const wrapper = mount(AppShell);
    await flushPromises();
    wrapper.findComponent(WorkspaceView).vm.$emit("jump-reference", jumpSql, "public.customers", jumpTarget);
    await vi.dynamicImportSettled();
    await flushPromises();
    expect(editorText(wrapper)).toBe(jumpSql);
    expect(wrapper.get(".q-banner").text()).toContain("这个连接已有正在执行的查询。");
    expect(wrapper.get(".q-banner").text()).toContain("SQL 草稿仍保留");
    wrapper.unmount();
  });

  it("shows the running export task in the status bar activity", async () => {
    const wrapper = mount(AppShell);
    const task: ExportTask = {
      id: "export-1",
      fileName: "orders.csv",
      path: "C:/tmp/orders.csv",
      status: "running",
      doneRows: 3,
      totalRows: 10,
      partial: false,
      message: "正在导出",
      residualPath: null,
    };
    exportTasks.push(task);
    await flushPromises();
    const activity = wrapper.get(".sb-activity");
    expect(activity.attributes("data-state")).toBe("running");
    expect(activity.text()).toContain("进行中");
    expect(activity.text()).toContain(taskStatusText(task));
    wrapper.unmount();
  });

  it("shows the running query in the status bar activity", async () => {
    const wrapper = mount(AppShell);
    await flushPromises();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "t", ctrlKey: true }));
    await flushPromises();
    const tabs = wrapper.findComponent(WorkspaceView).props("tabs") as ShellTab[];
    const tab = tabs[0]!;
    tab.connectionName = "书店";
    tab.database = "bookstore";
    adoptRun(tab.id, { ...doneRun(), status: "running" });
    await flushPromises();
    const activity = wrapper.get(".sb-activity");
    expect(activity.attributes("data-state")).toBe("running");
    expect(activity.text()).toContain("进行中");
    expect(activity.text()).toContain("正在执行查询：书店 / bookstore");
    wrapper.unmount();
  });
});
