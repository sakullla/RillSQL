import { reactive } from "vue";
import { gridExport, gridExportCancel, type ExportResult } from "../grid/api";
import type { Filter, SortSpec } from "../grid/types";
import { renderCsv, type CsvCell, type CsvOptions } from "./csv";
import { renderInsert, type InsertTarget } from "./insert";
import { renderJson } from "./json";
import { renderMarkdown } from "./markdown";
import type { ExportPlan } from "./plan";
import type { ExportFormat, WriteControl, WriteResult } from "./write";
import { isNativeExport, writeNativeCsv } from "./native";
import { renderXlsx } from "./xlsx";

export interface ExportTask {
  id: string;
  fileName: string;
  path: string;
  status: "running" | "done" | "failed" | "cancelled" | "blocked";
  doneRows: number;
  totalRows: number;
  partial: boolean;
  message: string;
  residualPath: string | null;
  cancellable?: boolean;
}

export const exportTasks = reactive<ExportTask[]>([]);

let taskSeq = 1;

export function resetExportTasks(): void {
  exportTasks.splice(0, exportTasks.length);
}

export function clearFinishedTasks(): void {
  for (let index = exportTasks.length - 1; index >= 0; index -= 1) {
    if (exportTasks[index]!.status !== "running") exportTasks.splice(index, 1);
  }
}

function applyWriteResult(task: ExportTask, result: WriteResult): void {
  task.status = result.status;
  task.doneRows = result.writtenRows;
  task.totalRows = Math.max(task.totalRows, result.writtenRows);
  task.message = result.message;
  task.residualPath = result.residualPath;
  if (result.path) {
    task.path = result.path;
    task.fileName = result.path.split(/[/\\]/).pop() || task.fileName;
  }
}

function blockedExport(message: string): WriteResult {
  return { status: "blocked", success: false, writtenRows: 0, path: null, residualPath: null, message, readCalled: false };
}

function downloadBytes(fileName: string, bytes: Uint8Array, type: string): void {
  const body = new Uint8Array(bytes);
  const blob = new Blob([body.buffer as ArrayBuffer], { type });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = fileName;
  document.body.append(link);
  link.click();
  link.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 30_000);
}

function downloadCsv(fileName: string, body: string): void {
  const blob = new Blob([body], { type: "text/csv;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = fileName;
  document.body.append(link);
  link.click();
  link.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 30_000);
}

export function renderExportText(options: {
  format?: ExportFormat;
  columns: readonly string[];
  rows: readonly Record<string, CsvCell>[];
  csv: CsvOptions;
  insert?: InsertTarget;
}): string {
  if (options.format === "xlsx") throw new Error("xlsx 不是文本");
  if (options.format === "json") return renderJson(options.columns, options.rows);
  if (options.format === "markdown") return renderMarkdown(options.columns, options.rows, { nullToken: options.csv.nullToken });
  if (options.format === "insert") {
    if (!options.insert?.name) throw new Error("还不能确定 INSERT 要写入的表");
    return renderInsert(options.insert, options.columns, options.rows);
  }
  return renderCsv(options.columns, options.rows, options.csv);
}

function exportLoadedInBrowser(options: {
  plan: ExportPlan;
  acknowledged: boolean;
  path: string;
  columns: readonly string[];
  rows: readonly Record<string, CsvCell>[];
  csv: CsvOptions;
  format?: ExportFormat;
  insert?: InsertTarget;
  readRows?: () => readonly Record<string, CsvCell>[];
}): WriteResult {
  if (!options.plan.allowed) return blockedExport(options.plan.rejectReason ?? "不能导出");
  if (options.plan.reexecutes && !options.acknowledged) return blockedExport("执行前需说明会重新执行这条 SELECT");
  if ((options.format ?? "csv") === "insert" && !options.insert?.name) return blockedExport("还不能确定 INSERT 要写入的表");
  let readCalled = false;
  let rows = options.rows;
  if (options.plan.serverRead) {
    if (!options.readRows) return blockedExport("没有可重新读取的数据源");
    readCalled = true;
    rows = options.readRows();
  }
  const fileName = options.path.split(/[/\\]/).pop() || options.plan.fileName;
  if ((options.format ?? "csv") === "xlsx") downloadBytes(fileName, renderXlsx(options.columns, rows), "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet");
  else downloadCsv(fileName, renderExportText({ ...options, rows }));
  let message = `已导出 ${rows.length} 行，开始下载 ${fileName}。`;
  if (options.plan.partial) message = `已导出 ${rows.length} 行，开始下载 ${fileName}。这只是已读取的行，不是完整结果。`;
  if (options.plan.reexecutes) message = `${options.plan.notice}${message}`;
  return { status: "done", success: true, writtenRows: rows.length, path: options.path, residualPath: null, message, readCalled };
}

async function startExportOnNode(
  options: {
    plan: ExportPlan;
    acknowledged: boolean;
    path: string;
    columns: readonly string[];
    rows: readonly Record<string, CsvCell>[];
    csv: CsvOptions;
    format?: ExportFormat;
    insert?: InsertTarget;
    control?: WriteControl;
    readRows?: () => readonly Record<string, CsvCell>[];
  },
  task: ExportTask,
): Promise<WriteResult> {
  const { startExport } = await import(/* @vite-ignore */ "./write");
  return startExport({
    ...options,
    onProgress: (progress) => {
      task.status = progress.status === "blocked" ? task.status : progress.status;
      task.doneRows = progress.writtenRows;
      task.totalRows = progress.totalRows;
    },
  });
}

export async function runExportTask(options: {
  plan: ExportPlan;
  acknowledged: boolean;
  path: string;
  columns: readonly string[];
  rows: readonly Record<string, CsvCell>[];
  csv: CsvOptions;
  format?: ExportFormat;
  insert?: InsertTarget;
  control?: WriteControl;
  readRows?: () => readonly Record<string, CsvCell>[];
}): Promise<{ task: ExportTask; result: WriteResult }> {
  const fileName = options.path.split(/[/\\]/).pop() || options.plan.fileName;
  const task = reactive<ExportTask>({
    id: `export-${taskSeq}`,
    fileName,
    path: options.path,
    status: "running",
    doneRows: 0,
    totalRows: options.rows.length,
    partial: options.plan.partial,
    message: "正在导出",
    cancellable: false,
    residualPath: null,
  });
  taskSeq += 1;
  exportTasks.unshift(task);
  let result: WriteResult;
  try {
    if (!options.plan.allowed) result = blockedExport(options.plan.rejectReason ?? "不能导出");
    else if (isNativeExport() && (options.format ?? "csv") !== "xlsx") {
      if (options.plan.serverRead) result = blockedExport("全量导出需要使用已打开的数据库连接。");
      else {
        await writeNativeCsv(options.path, renderExportText(options));
        result = {
          status: "done", success: true, writtenRows: options.rows.length, path: options.path,
          residualPath: null, readCalled: false,
          message: `已导出 ${options.rows.length} 行到 ${options.path}。${options.plan.partial ? "仅包含已读取的行，不是完整结果。" : ""}`,
        };
      }
    } else result = import.meta.env.VITEST ? await startExportOnNode(options, task) : exportLoadedInBrowser(options);
  } catch (error) {
    result = { status: "failed", success: false, writtenRows: 0, path: null, residualPath: null, readCalled: false,
      message: `导出失败：${error instanceof Error ? error.message : String(error)}` };
  }
  applyWriteResult(task, result);
  return { task, result };
}

export interface RemoteExportOptions {
  plan: ExportPlan;
  acknowledged: boolean;
  path: string;
  csv: CsvOptions;
  mode: "table-loaded" | "table-full" | "query";
  connectionId: string;
  database: string;
  schema: string;
  table: string;
  engine: string;
  filters: readonly Filter[];
  sort: SortSpec | null;
  limit: number;
  truncated: boolean;
  statement: string;
  format?: ExportFormat;
  insert?: InsertTarget;
}

function asStatus(status: string): ExportTask["status"] {
  if (status === "done" || status === "failed" || status === "cancelled" || status === "blocked" || status === "running") return status;
  return "failed";
}

function applyExportResult(task: ExportTask, result: ExportResult): void {
  task.status = asStatus(result.status);
  task.doneRows = result.writtenRows;
  task.totalRows = Math.max(task.totalRows, result.writtenRows);
  task.partial = result.partial;
  task.message = result.message;
  task.residualPath = result.residualPath;
  if (result.path) {
    task.path = result.path;
    task.fileName = result.path.split(/[/\\]/).pop() || task.fileName;
  }
  if (result.success && result.status !== "done") {
    task.status = "failed";
    task.message = result.message || "导出没有完成，不能记为成功";
  }
}

export function runRemoteExport(options: RemoteExportOptions): { task: ExportTask; done: Promise<ExportTask> } {
  const fileName = options.path.split(/[/\\]/).pop() || options.plan.fileName;
  const task = reactive<ExportTask>({
    id: `export-${taskSeq}`,
    fileName,
    path: options.path,
    status: "running",
    doneRows: 0,
    totalRows: options.mode === "table-loaded" ? options.limit : 0,
    partial: options.plan.partial,
    message: "正在从已打开的连接导出",
    residualPath: null,
  });
  taskSeq += 1;
  exportTasks.unshift(task);
  if ((options.format ?? "csv") === "xlsx") {
    task.status = "blocked";
    task.message = "没有生成文件：xlsx 只写出已经读取的行，完整重新导出不会生成文件。";
    return { task, done: Promise.resolve(task) };
  }
  const done = gridExport({
    id: task.id,
    mode: options.mode,
    connectionId: options.connectionId,
    database: options.database,
    schema: options.schema,
    table: options.table,
    engine: options.engine,
    filters: options.filters,
    sort: options.sort,
    limit: options.limit,
    truncated: options.truncated,
    statement: options.statement,
    acknowledged: options.acknowledged,
    path: options.path,
    delimiter: options.csv.delimiter,
    nullToken: options.csv.nullToken,
    bom: options.csv.bom,
    header: options.csv.header,
    format: options.format ?? "csv",
  })
    .then((result) => {
      applyExportResult(task, result);
      return task;
    })
    .catch((error: unknown) => {
      task.status = "failed";
      task.message = error instanceof Error ? error.message : String(error);
      return task;
    });
  return { task, done };
}

export function cancelRemoteExport(id: string): Promise<void> {
  return gridExportCancel(id);
}

/** 后台任务状态的直白文字：颜色不作唯一信息来源，状态词始终在。 */
export function taskStatusLabel(status: ExportTask["status"]): string {
  if (status === "running") return "进行中";
  if (status === "done") return "已完成";
  if (status === "cancelled") return "已取消";
  if (status === "blocked") return "未开始";
  return "失败";
}

export function taskStatusText(task: ExportTask): string {
  if (task.status === "running") {
    return task.totalRows > 0 ? `正在导出：已写入 ${task.doneRows} / ${task.totalRows} 行` : `正在导出：已写入 ${task.doneRows} 行`;
  }
  if (task.status === "done") return `已完成：写入 ${task.totalRows} 行${task.partial ? "，只是已读取的行，不是完整结果" : ""}`;
  if (task.status === "cancelled") return "已取消：未完成的文件已删除";
  if (task.status === "blocked") return `没有开始：${task.message}`;
  return `导出失败：${task.message || "原因未知"}`;
}

/** 已读取的行不再查询数据库。停在 0 行的旧任务改成未完成，标题不再一直显示「导出 0/N」。 */
export function failIdleRemoteExports(message: string): void {
  for (const task of exportTasks) {
    if (task.status !== "running" || task.doneRows !== 0 || task.totalRows <= 0) continue;
    if (!task.message.startsWith("正在从已打开的连接导出")) continue;
    task.status = "failed";
    task.message = message;
  }
}
