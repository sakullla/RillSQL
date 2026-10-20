import { invoke } from "@tauri-apps/api/core";

export function isNativeExport(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export async function chooseExportPath(defaultPath: string, format: "csv" | "json" | "insert" | "markdown" | "sql" = "csv"): Promise<string | null> {
  if (!isNativeExport()) return defaultPath;
  const { save } = await import("@tauri-apps/plugin-dialog");
  const filter = format === "json"
    ? { name: "JSON 文件", extensions: ["json"] }
    : format === "insert"
      ? { name: "SQL 文件", extensions: ["sql"] }
      : format === "markdown"
        ? { name: "Markdown 文件", extensions: ["md"] }
        : format === "sql"
          ? { name: "SQL 文件", extensions: ["sql"] }
          : { name: "CSV 文件", extensions: ["csv"] };
  const title = format === "json" ? "导出为 JSON" : format === "insert" ? "导出为 INSERT" : format === "markdown" ? "导出为 Markdown" : format === "sql" ? "保存 SQL 文件" : "导出为 CSV";
  return save({ title, defaultPath, filters: [filter] });
}

export function writeNativeCsv(path: string, contents: string): Promise<void> {
  return writeNativeFile(path, contents);
}

/** 把整段文本写进磁盘文件，与 CSV 导出共用同一条 IPC 通道。 */
export function writeNativeFile(path: string, contents: string): Promise<void> {
  return invoke<void>("export_write_loaded", { path, contents });
}
