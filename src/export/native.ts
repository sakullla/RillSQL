import { invoke } from "@tauri-apps/api/core";

export function isNativeExport(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export async function chooseExportPath(defaultPath: string): Promise<string | null> {
  if (!isNativeExport()) return defaultPath;
  const { save } = await import("@tauri-apps/plugin-dialog");
  return save({ title: "导出为 CSV", defaultPath, filters: [{ name: "CSV 文件", extensions: ["csv"] }] });
}

export function writeNativeCsv(path: string, contents: string): Promise<void> {
  return invoke<void>("export_write_loaded", { path, contents });
}
