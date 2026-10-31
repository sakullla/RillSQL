import type { Density, ThemePreference } from "./theme";

export type NullCopy = "NULL" | "empty" | "N";
/** SQL 编辑器配色：auto 跟随全局浅/深主题（现状默认配色），其余为 tokens.css 的命名变量组。 */
export type EditorTheme = "auto" | "contrast" | "warm" | "cool" | "mono";

export interface ShellSettings {
  theme: ThemePreference;
  density: Density;
  editorFont: number;
  editorTheme: EditorTheme;
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
  editorTheme: "auto",
  pageSize: 100,
  previewKB: 1,
  objectPage: 100,
  nullCopy: "NULL",
  queryTimeoutSec: 0,
};

const STORAGE_KEY = "rillsql.shell";

const DENSITIES: readonly Density[] = ["compact", "standard", "relaxed"];
export const EDITOR_THEMES: readonly EditorTheme[] = ["auto", "contrast", "warm", "cool", "mono"];

export function normalizeDensity(value: unknown): Density {
  if (value === "comfortable") return "standard"; // 历史版本把「标准」存成 comfortable
  return DENSITIES.includes(value as Density) ? (value as Density) : DEFAULT_SETTINGS.density;
}

export function normalizeEditorTheme(value: unknown): EditorTheme {
  return EDITOR_THEMES.includes(value as EditorTheme) ? (value as EditorTheme) : DEFAULT_SETTINGS.editorTheme;
}

export function loadSettings(storage: Storage | null = safeStorage()): ShellSettings {
  if (!storage) return { ...DEFAULT_SETTINGS };
  try {
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULT_SETTINGS };
    const parsed = JSON.parse(raw) as Omit<Partial<ShellSettings>, "density" | "editorTheme"> & { density?: string; editorTheme?: string };
    // 密度归一入口：历史值 comfortable 归到 standard，未知/缺失值归到默认档，避免样式选择器失配。
    const density: Density = normalizeDensity(parsed.density);
    // 编辑器配色同走归一：未知/缺失值归到 auto（跟随全局），避免 tokens.css 变量组选择器失配。
    const editorTheme: EditorTheme = normalizeEditorTheme(parsed.editorTheme);
    return { ...DEFAULT_SETTINGS, ...parsed, density, editorTheme };
  } catch {
    return { ...DEFAULT_SETTINGS };
  }
}

type SettingsSavedListener = (settings: ShellSettings) => void;
const savedListeners = new Set<SettingsSavedListener>();

/** 设置保存通知：QueryTab 这类不接收 settings 属性的深层组件借此即时响应（如编辑器配色切换），不引入全局状态。 */
export function onSettingsSaved(listener: SettingsSavedListener): () => void {
  savedListeners.add(listener);
  return () => {
    savedListeners.delete(listener);
  };
}

export function saveSettings(settings: ShellSettings, storage: Storage | null = safeStorage()): void {
  storage?.setItem(STORAGE_KEY, JSON.stringify(settings));
  const snapshot: ShellSettings = { ...settings };
  for (const listener of [...savedListeners]) listener(snapshot);
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
