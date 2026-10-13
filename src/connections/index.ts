export const MODULE_ID = "connections";

export interface PaletteConnection {
  id: string;
  label: string;
  path: string;
}

export function listPaletteConnections(): PaletteConnection[] {
  return [];
}

export async function pickSqliteFile(): Promise<string | null> {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    multiple: false,
    title: "打开 SQLite 文件",
    filters: [{ name: "SQLite", extensions: ["sqlite", "db", "sqlite3"] }],
  });
  if (picked === null) return null;
  return Array.isArray(picked) ? (picked[0] ?? null) : picked;
}
