import { invoke } from "@tauri-apps/api/core";

export function isNativeExport(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export async function chooseExportPath(defaultPath: string, format: "csv" | "json" | "insert" = "csv"): Promise<string | null> {
  if (!isNativeExport()) return defaultPath;
  const { save } = await import("@tauri-apps/plugin-dialog");
  const filter = format === "json"
    ? { name: "JSON 文件", extensions: ["json"] }
    : format === "insert"
      ? { name: "SQL 文件", extensions: ["sql"] }
      : { name: "CSV 文件", extensions: ["csv"] };
  const title = format === "json" ? "导出为 JSON" : format === "insert" ? "导出为 INSERT" : "导出为 CSV";
  return save({ title, defaultPath, filters: [filter] });
}

export function writeNativeCsv(path: string, contents: string): Promise<void> {
  return invoke<void>("export_write_loaded", { path, contents });
}
