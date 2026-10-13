import { shallowRef } from "vue";

export const MODULE_ID = "connections";

export interface PaletteConnection {
  id: string;
  label: string;
  path: string;
}

const palette = shallowRef<PaletteConnection[]>([]);

export function listPaletteConnections(): PaletteConnection[] {
  return palette.value;
}

export function replacePaletteConnections(connections: PaletteConnection[]): void {
  palette.value = connections;
}

async function defaultSqlitePicker(): Promise<string | null> {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    multiple: false,
    title: "打开 SQLite 文件",
    filters: [{ name: "SQLite", extensions: ["sqlite", "db", "sqlite3"] }],
  });
  if (picked === null) return null;
  return Array.isArray(picked) ? (picked[0] ?? null) : picked;
}

let sqlitePicker: () => Promise<string | null> = defaultSqlitePicker;

export function setSqlitePicker(picker: () => Promise<string | null>): void {
  sqlitePicker = picker;
}

export async function pickSqliteFile(): Promise<string | null> {
  return sqlitePicker();
}
