import { flushPromises, mount } from "@vue/test-utils";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import AppShell from "./AppShell.vue";

function mountAppShell() {
  const wrapper = mount(AppShell, {
    global: { stubs: { teleport: true } },
    attachTo: document.body,
  });
  return wrapper;
}

afterEach(() => {
  location.hash = "";
  delete document.documentElement.dataset.theme;
});

const tokens = readFileSync(resolve(dirname(fileURLToPath(import.meta.url)), "../styles/tokens.css"), "utf8");

function token(name: string): string {
  const match = tokens.match(new RegExp(`${name}\\s*:\\s*([^;]+);`));
  expect(match, `missing ${name}`).toBeTruthy();
  return match![1].trim();
}

describe("shell geometry (rendered)", () => {
  it("renders both title-bar rows and the menubar", async () => {
    const wrapper = mountAppShell();
    await flushPromises();
    expect(wrapper.find(".tb-menu-row").exists()).toBe(true);
    expect(wrapper.find(".tb-tool-row").exists()).toBe(true);
    expect(wrapper.find('[role="menubar"]').findAll("button").length).toBe(7);
    wrapper.unmount();
  });

  it("keeps the S7.2 layout tokens the shell CSS consumes", () => {
    for (const name of ["--titlebar-h", "--toolbar-h", "--tab-h", "--target-h", "--result-head-h", "--hint-h", "--status-h", "--rail-w"]) {
      expect(token(name), name).toMatch(/^\d+px$/);
    }
  });

  it("renders a status bar with a history segment", async () => {
    const wrapper = mountAppShell();
    await flushPromises();
    expect(wrapper.find(".statusbar").exists()).toBe(true);
    expect(wrapper.find('[data-segment="history"]').exists()).toBe(true);
    wrapper.unmount();
  });

  it("renders the toolbar with its primary run action", async () => {
    const wrapper = mountAppShell();
    await flushPromises();
    expect(wrapper.find('[role="toolbar"][aria-label="主工具栏"]').exists()).toBe(true);
    expect(wrapper.find('[role="toolbar"] [data-action="run"]').exists()).toBe(true);
    wrapper.unmount();
  });
});
