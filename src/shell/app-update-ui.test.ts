import { flushPromises, mount } from "@vue/test-utils";
import { expect, it, vi } from "vitest";
import type { UpdateHandle } from "./app-update";

const mocked = vi.hoisted(() => ({ check: vi.fn(), relaunch: vi.fn(async () => {}) }));
vi.mock("./window-chrome", () => ({ desktopShell: () => true }));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: mocked.check }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: mocked.relaunch }));
import AppUpdate from "./AppUpdate.vue";
import { setLocale } from "../i18n";
import { appUpdater } from "./app-update";

it("checks on request, renders release notes as text, switches language and installs explicitly", async () => {
  const update: UpdateHandle = { version: "0.2.4", body: "<script>bad()</script>", download: vi.fn(async (event) => {
    event({ event: "Started", data: {} });
    event({ event: "Progress", data: { chunkLength: 100 } });
  }), install: vi.fn(async () => {}), close: vi.fn(async () => {}) };
  mocked.check.mockResolvedValue(update);
  const prepare = vi.fn();
  const wrapper = mount(AppUpdate, { props: { autoCheck: true, prepare } });
  try {
    expect(mocked.check).not.toHaveBeenCalled();
    await wrapper.get("input[type=checkbox]").setValue(false);
    expect(wrapper.emitted("autoCheck")).toEqual([[false]]);
    await wrapper.get("button").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("0.2.4");
    expect(wrapper.find("script").exists()).toBe(false);
    expect(wrapper.get("pre").text()).toContain("<script>");
    expect(update.download).not.toHaveBeenCalled();
    setLocale("en");
    await flushPromises();
    expect(wrapper.text()).toContain("Version 0.2.4 is available");
    await wrapper.get("button").trigger("click");
    await flushPromises();
    expect(update.install).toHaveBeenCalledTimes(1);
    expect(prepare).toHaveBeenCalledTimes(3);
    expect(mocked.relaunch).toHaveBeenCalledTimes(1);
  } finally {
    wrapper.unmount();
    appUpdater.dispose();
    setLocale("zh-CN");
  }
});
