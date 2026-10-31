import { invoke } from "@tauri-apps/api/core";
import { tr } from "../i18n";

export function isNativeExport(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export async function chooseExportPath(defaultPath: string, format: "csv" | "json" | "insert" | "markdown" | "sql" | "xlsx" = "csv"): Promise<string | null> {
  if (!isNativeExport() || format === "xlsx") return defaultPath;
  const { save } = await import("@tauri-apps/plugin-dialog");
  const filter = format === "json"
    ? { name: tr("export.dialog.jsonFile"), extensions: ["json"] }
    : format === "insert"
      ? { name: tr("export.dialog.sqlFile"), extensions: ["sql"] }
      : format === "markdown"
        ? { name: tr("export.dialog.markdownFile"), extensions: ["md"] }
        : format === "sql"
          ? { name: tr("export.dialog.sqlFile"), extensions: ["sql"] }
          : { name: tr("export.dialog.csvFile"), extensions: ["csv"] };
  const title = tr(format === "json" ? "export.dialog.titleJson" : format === "insert" ? "export.dialog.titleInsert" : format === "markdown" ? "export.dialog.titleMarkdown" : format === "sql" ? "export.dialog.saveSqlTitle" : "export.dialog.titleCsv");
  return save({ title, defaultPath, filters: [filter] });
}

export function writeNativeCsv(path: string, contents: string): Promise<void> {
  return writeNativeFile(path, contents);
}

/** 把整段文本写进磁盘文件，与 CSV 导出共用同一条 IPC 通道。 */
export function writeNativeFile(path: string, contents: string): Promise<void> {
  return invoke<void>("export_write_loaded", { path, contents });
}
