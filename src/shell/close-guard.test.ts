import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { RunDto } from "../query/api";
import { adoptRun, attachCloser, resetQueryRuntime, tabRun } from "../query/runtime";
import AppShell from "./AppShell.vue";
import {
  anyPending,
  chooseLeave,
  chooseStay,
  closeFromTitleBar,
  closePrompt,
  handleCloseRequested,
  installCloseCoordinator,
  registerPending,
  resetCloseGuard,
  tabHasPending,
} from "./close-guard";
import { createSession } from "./session";
import TitleBar from "./TitleBar.vue";

const invoke = vi.hoisted(() => vi.fn());

type CloseEvent = { preventDefault: () => void; readonly defaultPrevented: boolean };

/**
 * Window.close resolves before tauri://close-requested. The hook is two
 * microtasks later so it runs after the title-bar await on close(), matching
 * Tauri 2.12 rather than invoking the handler inside close().
 */
const windowApi = vi.hoisted(() => {
  const api = {
    prevented: 0,
    destroyed: 0,
    closed: false,
    trace: [] as string[],
    handler: null as null | ((event: CloseEvent) => void | Promise<void>),
    close: vi.fn(() => {
      api.trace.push("close-return");
      let resolveClose: () => void = () => undefined;
      const promise = new Promise<void>((resolve) => {
        resolveClose = resolve;
      });
      queueMicrotask(() => {
        resolveClose();
        queueMicrotask(() => {
          queueMicrotask(() => {
            api.trace.push("hook");
            const handler = api.handler;
            if (!handler) {
              api.closed = true;
              return;
            }
            let prevented = false;
            const event: CloseEvent = {
              preventDefault() {
                prevented = true;
                api.prevented += 1;
              },
              get defaultPrevented() {
                return prevented;
              },
            };
            void handler(event);
          });
        });
      });
      return promise;
    }),
    onCloseRequested: vi.fn(async (handler: (event: CloseEvent) => void | Promise<void>) => {
      const wrapped = async (event: CloseEvent) => {
        await handler(event);
        if (!event.defaultPrevented) {
          api.trace.push("destroy");
          api.destroyed += 1;
          api.closed = true;
        }
      };
      api.handler = wrapped;
      return () => {
        if (api.handler === wrapped) api.handler = null;
      };
    }),
  };
  return api;
});

vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    minimize: async () => undefined,
    toggleMaximize: async () => undefined,
    close: windowApi.close,
    isMaximized: async () => false,
    isFocused: async () => true,
    onResized: async () => () => undefined,
    onFocusChanged: async () => () => undefined,
    onCloseRequested: windowApi.onCloseRequested,
  }),
}));

function running(runId: string): RunDto {
  return {
    id: runId,
    connectionId: "shop",
    connectionName: "Shop",
    database: "shop",
    engine: "sqlite",
    status: "running",
    cancel: "none",
    cancelMessage: "",
    viaSsh: false,
    freshChannel: false,
    statements: [],
    startedAt: "2026-10-16T00:00:00Z",
    elapsedMs: 1,
  };
}

function shellWithDraft(sql: string) {
  const session = createSession();
  installCloseCoordinator(session);
  attachCloser((id) => session.closeTab(id));
  session.openQuery();
  const id = session.activeId.value;
  if (!id) throw new Error("missing query tab");
  session.setDraft(id, sql);
  return { session, id };
}

beforeEach(() => {
  invoke.mockReset();
  invoke.mockRejectedValue(new Error("offline"));
  windowApi.prevented = 0;
  windowApi.destroyed = 0;
  windowApi.closed = false;
  windowApi.trace.length = 0;
  windowApi.handler = null;
  windowApi.close.mockClear();
  windowApi.onCloseRequested.mockClear();
  resetCloseGuard();
  resetQueryRuntime();
  localStorage.clear();
  location.hash = "";
});

afterEach(() => {
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
  resetCloseGuard();
  resetQueryRuntime();
  localStorage.clear();
  location.hash = "";
});

describe("close guard", () => {
  it("asks before closing a tab with uncommitted cell edits and can keep them", () => {
    const { session, id } = shellWithDraft("select 1");
    let dirty = true;
    registerPending(id, () => dirty);

    session.closeTab(id);
    expect(closePrompt.open).toBe(true);
    expect(closePrompt.scope).toBe("tab");
    expect(closePrompt.tabId).toBe(id);

    chooseStay();
    expect(closePrompt.open).toBe(false);
    expect(session.tabs.value.map((tab) => tab.id)).toEqual([id]);
    expect(session.tabs.value[0]?.draft).toBe("select 1");
    expect(tabHasPending(id)).toBe(true);
    expect(dirty).toBe(true);

    session.closeTab(id);
    expect(closePrompt.open).toBe(true);
    chooseLeave();
    expect(closePrompt.open).toBe(false);
    expect(session.tabs.value).toHaveLength(0);
    expect(tabHasPending(id)).toBe(false);
    expect(anyPending()).toBe(false);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("asks before closing a tab with an unexecuted SQL draft and can keep it", () => {
    const { session, id } = shellWithDraft("select unfinished");
    registerPending(id, () => false);
    expect(tabHasPending(id)).toBe(false);
    expect(anyPending()).toBe(true);

    session.closeTab(id);
    expect(closePrompt.open).toBe(true);
    expect(closePrompt.scope).toBe("tab");
    expect(closePrompt.tabId).toBe(id);
    expect(closePrompt.reason).toBe("draft");

    chooseStay();
    expect(closePrompt.open).toBe(false);
    expect(session.tabs.value.map((tab) => tab.id)).toEqual([id]);
    expect(session.tabs.value[0]?.draft).toBe("select unfinished");

    session.closeTab(id);
    expect(closePrompt.open).toBe(true);
    chooseLeave();
    expect(closePrompt.open).toBe(false);
    expect(session.tabs.value).toHaveLength(0);
    expect(anyPending()).toBe(false);
  });

  it("counts a non-empty SQL draft as pending for the window close", async () => {
    const { session, id } = shellWithDraft("select keep me");
    const event = { preventDefault: vi.fn() };

    const system = handleCloseRequested(event);
    await flushPromises();
    expect(closePrompt.open).toBe(true);
    expect(closePrompt.scope).toBe("window");
    expect(closePrompt.reason).toBe("draft");

    chooseStay();
    await system;
    expect(event.preventDefault).toHaveBeenCalledTimes(1);
    expect(session.tabs.value.map((tab) => tab.id)).toEqual([id]);
    expect(session.tabs.value[0]?.draft).toBe("select keep me");
  });

  it("drops the registration when the grid unregisters", () => {
    const { session, id } = shellWithDraft("");
    const stop = registerPending(id, () => true);
    expect(tabHasPending(id)).toBe(true);
    stop();
    expect(tabHasPending(id)).toBe(false);
    session.closeTab(id);
    expect(closePrompt.open).toBe(false);
    expect(session.tabs.value).toHaveLength(0);
  });

  it("keeps the tab when cancelling a running query fails", async () => {
    const { session, id } = shellWithDraft("select running");
    registerPending(id, () => true);
    adoptRun(id, running("run-close"));
    invoke.mockRejectedValue("取消失败：权限不足");

    session.closeTab(id);
    await flushPromises();

    expect(closePrompt.open).toBe(false);
    expect(session.tabs.value.map((tab) => tab.id)).toEqual([id]);
    expect(session.tabs.value[0]?.draft).toBe("select running");
    expect(tabRun(id)?.cancel).toBe("failed");
    expect(tabRun(id)?.status).toBe("running");
    expect(tabRun(id)?.cancelMessage).toContain("权限不足");
    expect(tabHasPending(id)).toBe(true);
  });

  it("prevents the system close when a running query cannot be cancelled", async () => {
    const { session, id } = shellWithDraft("select running");
    adoptRun(id, running("run-window"));
    invoke.mockRejectedValue("取消失败：权限不足");
    const event = { preventDefault: vi.fn() };

    await handleCloseRequested(event);
    await flushPromises();

    expect(event.preventDefault).toHaveBeenCalledTimes(1);
    expect(closePrompt.open).toBe(false);
    expect(session.tabs.value.map((tab) => tab.id)).toEqual([id]);
    expect(tabRun(id)?.cancel).toBe("failed");
    expect(windowApi.close).not.toHaveBeenCalled();
  });

  it("asks once for a window close and stay keeps the pending edits", async () => {
    const { session, id } = shellWithDraft("select 1");
    registerPending(id, () => true);
    const event = { preventDefault: vi.fn() };
    const native = vi.fn(async () => undefined);

    const system = handleCloseRequested(event);
    const title = closeFromTitleBar(native);
    await flushPromises();
    expect(closePrompt.open).toBe(true);
    expect(closePrompt.scope).toBe("window");

    chooseStay();
    await system;
    await title;

    expect(event.preventDefault).toHaveBeenCalledTimes(1);
    expect(native).not.toHaveBeenCalled();
    expect(session.tabs.value.map((tab) => tab.id)).toEqual([id]);
    expect(tabHasPending(id)).toBe(true);
    expect(session.tabs.value[0]?.draft).toBe("select 1");
  });

  it("lets the system close proceed after the user leaves, without a second prompt", async () => {
    registerPending("table:1", () => true);
    const event = { preventDefault: vi.fn() };
    const pending = handleCloseRequested(event);
    await flushPromises();
    expect(closePrompt.open).toBe(true);
    chooseLeave();
    await pending;
    expect(event.preventDefault).not.toHaveBeenCalled();
    expect(closePrompt.open).toBe(false);
    expect(tabHasPending("table:1")).toBe(true);
  });

  it("routes the title-bar close button through the same prompt", async () => {
    const { id } = shellWithDraft("");
    const title = mount(TitleBar, { props: { theme: "system", caption: true } });

    await title.get("[aria-label='关闭']").trigger("click");
    await flushPromises();
    expect(closePrompt.open).toBe(false);

    registerPending(id, () => true);
    await title.get("[aria-label='关闭']").trigger("click");
    await flushPromises();
    expect(closePrompt.open).toBe(true);
    expect(closePrompt.scope).toBe("window");
    chooseStay();
    expect(closePrompt.open).toBe(false);
    expect(tabHasPending(id)).toBe(true);
    expect(windowApi.close).not.toHaveBeenCalled();
    title.unmount();
  });

  it("does not close from the title bar when query cancel fails", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    const { session, id } = shellWithDraft("select running");
    adoptRun(id, running("run-title"));
    invoke.mockRejectedValue("取消失败：权限不足");
    const title = mount(TitleBar, { props: { theme: "system", caption: true } });
    await flushPromises();

    await title.get("[aria-label='关闭']").trigger("click");
    await flushPromises();

    expect(closePrompt.open).toBe(false);
    expect(session.tabs.value.map((tab) => tab.id)).toEqual([id]);
    expect(tabRun(id)?.cancel).toBe("failed");
    expect(windowApi.close).not.toHaveBeenCalled();
    expect(windowApi.destroyed).toBe(0);
    expect(windowApi.closed).toBe(false);
    title.unmount();
  });

  it("installs the system close hook and does not ask twice after the title bar confirms", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    registerPending("table:9", () => true);
    const wrapper = mount(AppShell);
    await flushPromises();
    expect(windowApi.onCloseRequested).toHaveBeenCalledTimes(1);
    expect(windowApi.handler).toBeTypeOf("function");

    await wrapper.get("[aria-label='关闭']").trigger("click");
    await flushPromises();
    expect(wrapper.get("#close-guard-title").text()).toBe("关闭窗口？");
    expect(wrapper.text()).toContain("不会写入数据库");
    expect(windowApi.close).not.toHaveBeenCalled();

    await wrapper.get("[data-action='close-stay']").trigger("click");
    expect(wrapper.find("#close-guard-title").exists()).toBe(false);
    expect(tabHasPending("table:9")).toBe(true);
    expect(windowApi.close).not.toHaveBeenCalled();

    await wrapper.get("[aria-label='关闭']").trigger("click");
    await flushPromises();
    await wrapper.get("[data-action='close-leave']").trigger("click");
    await flushPromises();

    expect(windowApi.close).toHaveBeenCalledTimes(1);
    expect(windowApi.trace).toEqual(["close-return", "hook", "destroy"]);
    expect(windowApi.prevented).toBe(0);
    expect(windowApi.destroyed).toBe(1);
    expect(windowApi.closed).toBe(true);
    expect(wrapper.find("#close-guard-title").exists()).toBe(false);
    wrapper.unmount();
  });

  it("keeps a tab in the shell dialog when cell edits are pending", async () => {
    const wrapper = mount(AppShell);
    await flushPromises();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "t", ctrlKey: true }));
    await flushPromises();
    const tab = wrapper.get("[data-tab-id]");
    const id = tab.attributes("data-tab-id");
    expect(id).toBeTruthy();
    registerPending(id!, () => true);

    await wrapper.get(".tab-x").trigger("click");
    await flushPromises();
    expect(wrapper.get("#close-guard-title").text()).toBe("关闭这个标签？");
    expect(wrapper.text()).toContain("留下则标签和修改都还在");
    await wrapper.get("[data-action='close-stay']").trigger("click");
    expect(wrapper.find(`[data-tab-id='${id}']`).exists()).toBe(true);
    expect(tabHasPending(id!)).toBe(true);

    await wrapper.get(".tab-x").trigger("click");
    await flushPromises();
    await wrapper.get("[data-action='close-leave']").trigger("click");
    expect(wrapper.find(`[data-tab-id='${id}']`).exists()).toBe(false);
    expect(invoke.mock.calls.map((call) => call[0])).toEqual(["app_info", "catalog_sessions"]);
    wrapper.unmount();
  });

  it("grants destroy and no other extra window permission", () => {
    const permissions = (
      JSON.parse(readFileSync(resolve("src-tauri/capabilities/default.json"), "utf8")) as {
        permissions: string[];
      }
    ).permissions;
    expect(permissions.filter((item) => item.startsWith("core:window:"))).toEqual([
      "core:window:allow-minimize",
      "core:window:allow-toggle-maximize",
      "core:window:allow-close",
      "core:window:allow-destroy",
      "core:window:allow-start-dragging",
    ]);
  });

  function closeResolvesBeforeHook(runHook: () => void): Promise<void> {
    let resolveClose: () => void = () => undefined;
    const promise = new Promise<void>((resolve) => {
      resolveClose = resolve;
    });
    queueMicrotask(() => {
      resolveClose();
      queueMicrotask(() => {
        queueMicrotask(runHook);
      });
    });
    return promise;
  }

  it("does not ask again when the system hook runs after close() resolves", async () => {
    registerPending("table:late", () => true);
    const event = { preventDefault: vi.fn() };
    const native = vi.fn(() => closeResolvesBeforeHook(() => void handleCloseRequested(event)));

    const title = closeFromTitleBar(native);
    await flushPromises();
    expect(closePrompt.open).toBe(true);
    expect(closePrompt.scope).toBe("window");
    expect(native).not.toHaveBeenCalled();

    chooseLeave();
    await title;
    await flushPromises();

    expect(native).toHaveBeenCalledTimes(1);
    expect(event.preventDefault).not.toHaveBeenCalled();
    expect(closePrompt.open).toBe(false);
    expect(tabHasPending("table:late")).toBe(true);
  });

  it("finishes the window close after close() returns when nothing is pending", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    const title = mount(TitleBar, { props: { theme: "system", caption: true } });
    await flushPromises();

    await title.get("[aria-label='关闭']").trigger("click");
    await flushPromises();

    expect(closePrompt.open).toBe(false);
    expect(windowApi.trace[0]).toBe("close-return");
    expect(windowApi.trace.indexOf("hook")).toBeGreaterThan(windowApi.trace.indexOf("close-return"));
    expect(windowApi.trace).toEqual(["close-return", "hook", "destroy"]);
    expect(windowApi.prevented).toBe(0);
    expect(windowApi.closed).toBe(true);
    title.unmount();
  });

  it("does not destroy the window when a system close cannot cancel a query", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
    const { session, id } = shellWithDraft("select running");
    adoptRun(id, running("run-os-close"));
    invoke.mockRejectedValue("取消失败：权限不足");
    const title = mount(TitleBar, { props: { theme: "system", caption: true } });
    await flushPromises();

    const closing = windowApi.close();
    expect(windowApi.trace).toEqual(["close-return"]);
    expect(windowApi.closed).toBe(false);
    await closing;
    await flushPromises();

    expect(windowApi.trace).toEqual(["close-return", "hook"]);
    expect(windowApi.prevented).toBe(1);
    expect(windowApi.destroyed).toBe(0);
    expect(windowApi.closed).toBe(false);
    expect(closePrompt.open).toBe(false);
    expect(session.tabs.value.map((tab) => tab.id)).toEqual([id]);
    expect(tabRun(id)?.cancel).toBe("failed");
    title.unmount();
  });
});
