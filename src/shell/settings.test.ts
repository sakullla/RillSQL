import { mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it } from "vitest";
import { DEFAULT_SETTINGS, loadSettings, saveSettings } from "./settings";
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

  it("defaults the statement timeout to unlimited and keeps stored overrides", () => {
    expect(DEFAULT_SETTINGS.queryTimeoutSec).toBe(0);
    localStorage.setItem("rillsql.shell", JSON.stringify({ pageSize: 50 }));
    expect(loadSettings().queryTimeoutSec).toBe(0);
    localStorage.setItem("rillsql.shell", JSON.stringify({ queryTimeoutSec: 30 }));
    expect(loadSettings().queryTimeoutSec).toBe(30);
  });

  it("round-trips through saveSettings", () => {
    const storage = fakeStorage();
    const settings = { ...DEFAULT_SETTINGS, density: "standard" as const, queryTimeoutSec: 60 };
    saveSettings(settings, storage);
    expect(loadSettings(storage)).toEqual(settings);
  });

  it("changes font, page size, preview, object count and null text through in-app listboxes", async () => {
    const wrapper = mount(SettingsView, { props: { settings: { ...DEFAULT_SETTINGS }, dataDir: null } });
    expect(wrapper.find("select").exists()).toBe(false);
    expect(wrapper.find('input[type="radio"][name="set-theme"]').exists()).toBe(true);
    expect(wrapper.find('input[type="radio"][name="set-density"]').exists()).toBe(true);
    expect(wrapper.find(".btn.primary").exists()).toBe(false);

    const cases = [
      { label: "编辑器字号", current: "13 px", option: "15 px", patch: { editorFont: 15 } },
      { label: "每次读取的行数", current: "100 行", option: "50 行", patch: { pageSize: 50 } },
      { label: "大字段预览上限", current: "1 KB", option: "4 KB", patch: { previewKB: 4 } },
      { label: "对象列表每次显示", current: "100 个", option: "200 个", patch: { objectPage: 200 } },
      { label: "语句执行超时", current: "不限制", option: "30 秒", patch: { queryTimeoutSec: 30 } },
      { label: "复制 NULL 时写入", current: "NULL", option: "空文本", patch: { nullCopy: "empty" } },
    ];
    for (const item of cases) {
      const trigger = wrapper.get(`[role="combobox"][aria-label="${item.label}"]`);
      expect(trigger.attributes("aria-expanded")).toBe("false");
      expect(trigger.text()).toBe(item.current);
      await trigger.trigger("click");
      expect(wrapper.find("select").exists()).toBe(false);
      const option = wrapper.findAll('[role="option"]').find((node) => node.text() === item.option);
      expect(option, item.option).toBeTruthy();
      await option!.trigger("click");
      expect(wrapper.emitted("change")?.at(-1)).toEqual([item.patch]);
      expect(wrapper.find('[role="listbox"]').exists()).toBe(false);
    }
    wrapper.unmount();
  });
});
