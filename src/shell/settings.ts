import type { Density, ThemePreference } from "./theme";

export type NullCopy = "NULL" | "empty" | "N";

export interface ShellSettings {
  theme: ThemePreference;
  density: Density;
  editorFont: number;
  pageSize: number;
  previewKB: number;
  objectPage: number;
  nullCopy: NullCopy;
  /** 单条语句的执行超时（秒）。0 表示不限制。 */
  queryTimeoutSec: number;
}

export const DEFAULT_SETTINGS: ShellSettings = {
  theme: "system",
  density: "compact",
  editorFont: 13,
  pageSize: 100,
  previewKB: 1,
  objectPage: 100,
  nullCopy: "NULL",
  queryTimeoutSec: 0,
};

const STORAGE_KEY = "rillsql.shell";

const DENSITIES: readonly Density[] = ["compact", "standard", "relaxed"];

export function normalizeDensity(value: unknown): Density {
  if (value === "comfortable") return "standard"; // 历史版本把「标准」存成 comfortable
  return DENSITIES.includes(value as Density) ? (value as Density) : DEFAULT_SETTINGS.density;
}

export function loadSettings(storage: Storage | null = safeStorage()): ShellSettings {
  if (!storage) return { ...DEFAULT_SETTINGS };
  try {
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULT_SETTINGS };
    const parsed = JSON.parse(raw) as Omit<Partial<ShellSettings>, "density"> & { density?: string };
    // 密度归一入口：历史值 comfortable 归到 standard，未知/缺失值归到默认档，避免样式选择器失配。
    const density: Density = normalizeDensity(parsed.density);
    return { ...DEFAULT_SETTINGS, ...parsed, density };
  } catch {
    return { ...DEFAULT_SETTINGS };
  }
}

export function saveSettings(settings: ShellSettings, storage: Storage | null = safeStorage()): void {
  storage?.setItem(STORAGE_KEY, JSON.stringify(settings));
}

function safeStorage(): Storage | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

export function localDataPathLabel(mac: boolean, linux: boolean): string {
  if (mac) return "~/Library/Application Support/RillSQL/";
  if (linux) return "~/.local/share/rillsql/";
  return "%APPDATA%\\RillSQL\\";
}
