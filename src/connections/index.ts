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

async function defaultExportPathPicker(encrypted: boolean): Promise<string | null> {
  const { save } = await import("@tauri-apps/plugin-dialog");
  return save({
    title: encrypted ? "导出加密的连接文件" : "导出连接文件",
    defaultPath: encrypted ? "rillsql-connections.rillsqlcx" : "rillsql-connections.json",
    filters: [{ name: "RillSQL 连接文件", extensions: ["json", "rillsqlcx"] }],
  });
}

let exportPathPicker: (encrypted: boolean) => Promise<string | null> = defaultExportPathPicker;

/** 测试注入点：导出路径来自原生保存对话框（dialog:default 已在 capabilities 内）。 */
export function setExportPathPicker(picker: (encrypted: boolean) => Promise<string | null>): void {
  exportPathPicker = picker;
}

export async function pickExportPath(encrypted: boolean): Promise<string | null> {
  return exportPathPicker(encrypted);
}

async function defaultConnectionsFilePicker(): Promise<string | null> {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    multiple: false,
    title: "选择 RillSQL 连接文件",
    filters: [{ name: "RillSQL 连接文件", extensions: ["json", "rillsqlcx"] }],
  });
  if (picked === null) return null;
  return Array.isArray(picked) ? (picked[0] ?? null) : picked;
}

let connectionsFilePicker: () => Promise<string | null> = defaultConnectionsFilePicker;

/** 测试注入点：RillSQL 文件导入走原生对话框选文件。 */
export function setConnectionsFilePicker(picker: () => Promise<string | null>): void {
  connectionsFilePicker = picker;
}

export async function pickConnectionsFile(): Promise<string | null> {
  return connectionsFilePicker();
}
