import { invoke } from "@tauri-apps/api/core";
import type { Cell } from "../grid/types";
import { isNativeExport } from "./native";
import { parseImportCsv, parseImportJson, type ImportSeed, type ParsedImport } from "./import";

/** 拖放预览一次读取的上限：与后端 dropped.rs 的 2 MB 硬上限一致，对齐 file-io.ts 的档位。 */
export const MAX_DROPPED_PREVIEW_BYTES = 2 * 1024 * 1024;

/** read_dropped_preview 的返回：text 已按行边界截断，truncated 表示文件还有未读内容。 */
export interface DroppedPreview {
  path: string;
  format: "csv" | "json";
  text: string;
  fileSize: number;
  readBytes: number;
  truncated: boolean;
}

/** Tauri 原生 drag-drop 事件的形状：enter/over/drop 带 paths（over 只有位置），leave 什么都没有。 */
export type DragDropEventLike = { type: "enter" | "over" | "leave" | "drop"; paths?: readonly string[] | null };

export type DroppedFileState =
  | { kind: "unsupported"; extension: string; fileName: string }
  | { kind: "error"; fileName: string; message: string }
  | { kind: "ready"; fileName: string; preview: DroppedPreview; parsed: ParsedImport };

export function fileNameOf(path: string): string {
  return path.split(/[/\\]/).pop() || path;
}

export function extensionOf(path: string): string {
  const name = fileNameOf(path);
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
}

/** 扩展名分流：仅 .csv/.json 进入预览，其余（含 XLSX/Parquet）明确不支持（R9）。 */
export function routeDroppedFile(path: string): { kind: "preview"; format: "csv" | "json"; fileName: string } | { kind: "unsupported"; extension: string; fileName: string } {
  const extension = extensionOf(path);
  if (extension === "csv" || extension === "json") return { kind: "preview", format: extension, fileName: fileNameOf(path) };
  return { kind: "unsupported", extension, fileName: fileNameOf(path) };
}

/** 不支持格式的提示文案：含格式名；电子表格补充仍可从导入对话框进入。 */
export function unsupportedDropText(extension: string): string {
  const name = extension ? `.${extension} 文件` : "没有扩展名的文件";
  const spreadsheet = extension === "xlsx" || extension === "xls" ? "Excel 文件仍可以从数据网格的导入对话框进入。" : "";
  return `不支持拖放 ${name}：拖放预览只支持 CSV 与 JSON。${spreadsheet}`;
}

export function readDroppedPreview(path: string, limit: number = MAX_DROPPED_PREVIEW_BYTES): Promise<DroppedPreview> {
  return invoke<DroppedPreview>("read_dropped_preview", { path, limit });
}

/** 预览行预算：沿用「大字段预览上限」设置（previewKB，默认 1 KB）换算成字节。 */
export function previewBudgetBytes(previewKB: number): number {
  return Math.max(1, Math.floor(previewKB)) * 1024;
}

function rowBytes(row: Record<string, Cell>): number {
  let total = 0;
  for (const value of Object.values(row)) total += (value?.length ?? 0) + 1;
  return total;
}

/** 在字节预算内取前 N 行：至少显示一行，返回可见行与被折叠的行数。 */
export function previewRowsWithin(rows: readonly Record<string, Cell>[], budgetBytes: number): { rows: Record<string, Cell>[]; hidden: number } {
  const visible: Record<string, Cell>[] = [];
  let used = 0;
  for (const row of rows) {
    const size = rowBytes(row);
    if (visible.length > 0 && used + size > budgetBytes) break;
    visible.push(row);
    used += size;
  }
  return { rows: visible, hidden: Math.max(0, rows.length - visible.length) };
}

export function formatDropBytes(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} B`;
}

/**
 * 拖放事件分流：enter/over 点亮遮罩、leave 熄灭；drop 按扩展名分流——
 * .csv/.json 限量读取并解析出预览，其余直接给明确的不支持提示；读取/解析失败给错误态。
 */
export function createDroppedFileHandler(options: {
  setState: (state: DroppedFileState | null) => void;
  setHovering: (hovering: boolean) => void;
  read?: (path: string) => Promise<DroppedPreview>;
}): (event: DragDropEventLike) => void {
  const read = options.read ?? readDroppedPreview;
  return (event) => {
    if (event.type === "enter" || event.type === "over") {
      options.setHovering(true);
      return;
    }
    options.setHovering(false);
    if (event.type === "leave") return;
    const path = event.paths?.[0];
    if (!path) return;
    const route = routeDroppedFile(path);
    if (route.kind === "unsupported") {
      options.setState(route);
      return;
    }
    read(path).then(
      (preview) => {
        const outcome = preview.format === "json" ? parseImportJson(preview.text) : parseImportCsv(preview.text);
        if (!outcome.ok) {
          options.setState({ kind: "error", fileName: route.fileName, message: `${outcome.reason}文件可能不是这种格式，或已损坏。` });
          return;
        }
        options.setState({ kind: "ready", fileName: route.fileName, preview, parsed: outcome.data });
      },
      (cause) => {
        options.setState({ kind: "error", fileName: route.fileName, message: cause instanceof Error ? cause.message : String(cause) });
      },
    );
  };
}

/** 在 Tauri 原生环境挂接 drag-drop 事件；浏览器开发模式返回 null，不监听（原生行为在 Tauri 内单独核验）。 */
export async function connectDroppedFiles(handler: (event: DragDropEventLike) => void): Promise<(() => void) | null> {
  if (!isNativeExport()) return null;
  try {
    const { getCurrentWebview } = await import("@tauri-apps/api/webview");
    // EventCallback 收到的是 Event 信封，载荷在 event.payload 上。
    return await getCurrentWebview().onDragDropEvent((event) => handler(event.payload));
  } catch {
    return null;
  }
}

export type { ImportSeed };
