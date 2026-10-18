import { beforeEach, describe, expect, it } from "vitest";
import { DEFAULT_SETTINGS, loadSettings, saveSettings } from "./settings";

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
