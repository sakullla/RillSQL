import { mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it } from "vitest";
import { DEFAULT_SETTINGS, loadSettings, saveSettings, type ShellSettings } from "./settings";
import SettingsView from "./SettingsView.vue";

function fakeStorage(): Storage {
  const map = new Map<string, string>();
  return {
    get length() { return map.size; },
    clear: () => map.clear(),
    getItem: (key: string) => map.get(key) ?? null,
    key: (index: number) => [...map.keys()][index] ?? null,
    removeItem: (key: string) => void map.delete(key),
    setItem: (key: string, value: string) => void map.set(key, value),
  } as Storage;
}

describe("shell settings", () => {
  beforeEach(() => localStorage.clear());

  it("migrates the legacy comfortable density to standard", () => {
    localStorage.setItem("rillsql.shell", JSON.stringify({ theme: "dark", density: "comfortable" }));
    expect(loadSettings()).toMatchObject({ theme: "dark", density: "standard" });
  });

  it("keeps compact and passes stored values through", () => {
    localStorage.setItem("rillsql.shell", JSON.stringify({ density: "compact", pageSize: 50 }));
    expect(loadSettings()).toMatchObject({ density: "compact", pageSize: 50 });
  });

  it("falls back to compact by default", () => {
    expect(loadSettings()).toEqual(DEFAULT_SETTINGS);
    expect(DEFAULT_SETTINGS.density).toBe("compact");
  });

  it("includes TSV headers by default and merges old stored JSON", () => {
    expect(DEFAULT_SETTINGS.tsvHeaders).toBe(true);
    expect(loadSettings().tsvHeaders).toBe(true);
    localStorage.setItem("rillsql.shell", JSON.stringify({ theme: "dark", pageSize: 20 }));
    expect(loadSettings()).toMatchObject({ theme: "dark", pageSize: 20, tsvHeaders: true });
  });

  it("round-trips through saveSettings", () => {
    const storage = fakeStorage();
    const settings = { ...DEFAULT_SETTINGS, density: "standard" as const };
    saveSettings(settings, storage);
    expect(loadSettings(storage)).toEqual(settings);
  });
});

function mountSettings(overrides: Partial<ShellSettings> = {}) {
  return mount(SettingsView, {
    props: { settings: { ...DEFAULT_SETTINGS, tsvHeaders: false, ...overrides }, dataDir: null },
  });
}

describe("settings view layout and controls", () => {
  it("renders pickers as Listbox comboboxes instead of native selects", () => {
    const wrapper = mountSettings();
    expect(wrapper.find("select").exists()).toBe(false);
    const labels = wrapper
      .findAll('button[role="combobox"]')
      .map((button) => button.attributes("aria-label"));
    expect(labels).toContain("编辑器字号");
    expect(labels).toContain("每次读取的行数");
    expect(labels).toContain("大字段预览上限");
    expect(labels).toContain("对象列表每次显示");
    expect(labels).toContain("复制 NULL 时写入");
  });

  it("exposes tsvHeaders as a switch that emits the toggled value", async () => {
    const wrapper = mountSettings();
    const toggle = wrapper.get('[role="switch"]');
    expect(toggle.attributes("aria-label")).toContain("表头");
    expect(toggle.attributes("aria-checked")).toBe("false");
    await toggle.trigger("click");
    expect(wrapper.emitted("change")).toEqual([[{ tsvHeaders: true }]]);
  });

  it("uses a segmented radiogroup for density", async () => {
    const wrapper = mountSettings();
    const group = wrapper.get('[role="radiogroup"]');
    expect(group.attributes("aria-label")).toBe("界面密度");
    const radios = group.findAll('[role="radio"]');
    expect(radios.map((radio) => radio.text())).toEqual(["紧凑", "标准"]);
    await radios[1]!.trigger("click");
    expect(wrapper.emitted("change")).toEqual([[{ density: "standard" }]]);
  });

  it("keeps a labelled side navigation for the settings sections", async () => {
    const wrapper = mountSettings();
    const nav = wrapper.get('nav[aria-label="设置分区"]');
    const items = nav.findAll("button");
    expect(items.map((item) => item.text())).toEqual(["外观与编辑器", "数据读取", "本地数据", "关于"]);
    expect(items[0]!.classes()).toContain("on");
    await items[1]!.trigger("click");
    expect(items[1]!.classes()).toContain("on");
    expect(items[0]!.classes()).not.toContain("on");
  });

  it("keeps the theme preview thumbnails and the about card", () => {
    const wrapper = mountSettings();
    expect(wrapper.findAll(".theme-card")).toHaveLength(3);
    expect(wrapper.findAll(".tc-prev")).toHaveLength(3);
    expect(wrapper.text()).toContain("澄库 · RillSQL");
  });
});
