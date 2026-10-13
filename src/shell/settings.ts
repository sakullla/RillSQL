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
  typeToConfirm: boolean;
  confirmClose: boolean;
}

export const DEFAULT_SETTINGS: ShellSettings = {
  theme: "system",
  density: "compact",
  editorFont: 13,
  pageSize: 100,
  previewKB: 1,
  objectPage: 100,
  nullCopy: "NULL",
  typeToConfirm: true,
  confirmClose: true,
};

const STORAGE_KEY = "rillsql.shell";

export function loadSettings(storage: Storage | null = safeStorage()): ShellSettings {
  if (!storage) return { ...DEFAULT_SETTINGS };
  try {
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULT_SETTINGS };
    const parsed = JSON.parse(raw) as Partial<ShellSettings>;
    return { ...DEFAULT_SETTINGS, ...parsed };
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
